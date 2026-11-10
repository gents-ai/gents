// ax: drive a running macOS app through the Accessibility API, for first-run
// tests of a released build that has no WebDriver. Needs the Accessibility
// permission for the process that launches it (sshd-keygen-wrapper over SSH).
//
//   ax dump  <app>                          one JSON line per element
//   ax click <app> <selector>               AXPress the first match
//   ax type  <app> <selector> <text>        focus, select all, type
//   ax wait  <app> <selector> [seconds]     until a match exists
//   ax text  <app>                          all visible text, one line each
//   ax value <app> <selector>               the first match's value
//   ax drag  <app> <from> <to>              mouse-drag one element onto another
//
// <app> is a localized app name or a pid. <selector> is `#dom-id`,
// `role=Name` (AXButton=Next, AXRadioButton=~Local for a substring, `*` for
// any role), or `text=Exact line`.
import AppKit
import ApplicationServices
import Foundation

struct Node {
  let element: AXUIElement
  let role: String
  let name: String
  let domId: String
  let value: String
}

func attr(_ el: AXUIElement, _ key: String) -> AnyObject? {
  var out: AnyObject?
  return AXUIElementCopyAttributeValue(el, key as CFString, &out) == .success ? out : nil
}

func string(_ el: AXUIElement, _ key: String) -> String {
  guard let v = attr(el, key) else { return "" }
  if let s = v as? String { return s }
  if let n = v as? NSNumber { return n.stringValue }
  return ""
}

func walk(_ root: AXUIElement, depth: Int = 0, into nodes: inout [Node]) {
  guard depth < 80 else { return }
  let title = string(root, kAXTitleAttribute)
  let desc = string(root, kAXDescriptionAttribute)
  nodes.append(Node(
    element: root,
    role: string(root, kAXRoleAttribute),
    name: title.isEmpty ? desc : title,
    domId: string(root, "AXDOMIdentifier"),
    value: string(root, kAXValueAttribute)))
  if let children = attr(root, kAXChildrenAttribute) as? [AXUIElement] {
    for child in children { walk(child, depth: depth + 1, into: &nodes) }
  }
}

func appPid(_ app: String) -> pid_t? {
  if let pid = Int32(app) { return pid }
  return NSWorkspace.shared.runningApplications.first { $0.localizedName == app }?.processIdentifier
}

// WebKit builds a web view's accessibility tree only once an assistive client
// announces itself, as VoiceOver does with these attributes on the app.
var announced = Set<pid_t>()
func announce(_ app: AXUIElement, _ pid: pid_t) {
  guard !announced.contains(pid) else { return }
  announced.insert(pid)
  for name in ["AXEnhancedUserInterface", "AXManualAccessibility"] {
    AXUIElementSetAttributeValue(app, name as CFString, kCFBooleanTrue)
  }
  usleep(500_000)
}

func nodes(_ app: String) -> [Node] {
  guard let pid = appPid(app) else { return [] }
  let root = AXUIElementCreateApplication(pid)
  announce(root, pid)
  var out: [Node] = []
  walk(root, into: &out)
  return out
}

func matches(_ n: Node, _ selector: String) -> Bool {
  if selector.hasPrefix("#") { return n.domId == String(selector.dropFirst()) }
  if selector.hasPrefix("text=") {
    let want = String(selector.dropFirst(5))
    return n.value.trimmingCharacters(in: .whitespacesAndNewlines) == want
  }
  let parts = selector.split(separator: "=", maxSplits: 1, omittingEmptySubsequences: false).map(String.init)
  guard parts.count == 2, parts[0] == "*" || n.role == parts[0] else { return false }
  let label = n.name.isEmpty ? n.value : n.name
  if parts[1].hasPrefix("~") { return label.localizedCaseInsensitiveContains(String(parts[1].dropFirst())) }
  return label.trimmingCharacters(in: .whitespacesAndNewlines) == parts[1]
}

func find(_ app: String, _ selector: String) -> Node? { nodes(app).first { matches($0, selector) } }

func json(_ n: Node) -> String {
  let fields = ["role": n.role, "name": n.name, "id": n.domId, "value": String(n.value.prefix(120))]
  let data = try! JSONSerialization.data(withJSONObject: fields, options: [.sortedKeys])
  return String(data: data, encoding: .utf8)!
}

func typeText(_ text: String, pid: pid_t) {
  for scalar in text.utf16 {
    for down in [true, false] {
      let e = CGEvent(keyboardEventSource: nil, virtualKey: 0, keyDown: down)!
      var unit = scalar
      e.keyboardSetUnicodeString(stringLength: 1, unicodeString: &unit)
      e.postToPid(pid)
    }
    usleep(4_000)
  }
}

func center(_ el: AXUIElement) -> CGPoint? {
  guard let p = attr(el, kAXPositionAttribute), let z = attr(el, kAXSizeAttribute) else { return nil }
  var point = CGPoint.zero, size = CGSize.zero
  AXValueGetValue(p as! AXValue, .cgPoint, &point)
  AXValueGetValue(z as! AXValue, .cgSize, &size)
  return CGPoint(x: point.x + size.width / 2, y: point.y + size.height / 2)
}

func mouse(_ type: CGEventType, _ at: CGPoint) {
  CGEvent(mouseEventSource: nil, mouseType: type, mouseCursorPosition: at, mouseButton: .left)!.post(tap: .cghidEventTap)
}

func fail(_ message: String) -> Never {
  FileHandle.standardError.write((message + "\n").data(using: .utf8)!)
  exit(1)
}

let args = CommandLine.arguments
guard args.count >= 3 else { fail("usage: ax dump|click|type|wait|text <app> [selector] [text|seconds]") }
guard AXIsProcessTrusted() else { fail("ax: this process lacks the Accessibility permission") }
let (command, app) = (args[1], args[2])

switch command {
case "dump":
  for n in nodes(app) { print(json(n)) }
case "text":
  for n in nodes(app) where n.role == "AXStaticText" && !n.value.isEmpty { print(n.value) }
case "drag":
  guard args.count >= 5, let a = find(app, args[3]), let b = find(app, args[4]),
        let from = center(a.element), let to = center(b.element) else { fail("ax: drag needs two elements with positions") }
  mouse(.mouseMoved, from); usleep(200_000)
  mouse(.leftMouseDown, from); usleep(300_000)
  for step in 1...30 {
    let t = CGFloat(step) / 30
    mouse(.leftMouseDragged, CGPoint(x: from.x + (to.x - from.x) * t, y: from.y + (to.y - from.y) * t))
    usleep(25_000)
  }
  usleep(300_000)
  mouse(.leftMouseUp, to)
case "value":
  guard args.count >= 4, let n = find(app, args[3]) else { fail("ax: no element for \(args.dropFirst(3).first ?? "")") }
  print(n.value)
case "set":
  guard args.count >= 5, let n = find(app, args[3]) else { fail("ax: no element for \(args.dropFirst(3).first ?? "")") }
  let result = AXUIElementSetAttributeValue(n.element, kAXValueAttribute as CFString, args[4] as CFString)
  if result != .success { fail("ax: setting the value failed (\(result.rawValue)) on \(json(n))") }
case "click":
  guard args.count >= 4, let n = find(app, args[3]) else { fail("ax: no element for \(args.dropFirst(3).first ?? "")") }
  let result = AXUIElementPerformAction(n.element, kAXPressAction as CFString)
  if result != .success { fail("ax: press failed (\(result.rawValue)) on \(json(n))") }
case "type":
  guard args.count >= 5, let n = find(app, args[3]), let pid = appPid(app) else { fail("ax: no element for \(args[3])") }
  AXUIElementSetAttributeValue(n.element, kAXFocusedAttribute as CFString, kCFBooleanTrue)
  usleep(100_000)
  // Select the whole current value so typing replaces it, the way a user's
  // select-all does; key-combination events posted to a pid don't reach WebKit.
  var all = CFRange(location: 0, length: (n.value as NSString).length)
  if let range = AXValueCreate(.cfRange, &all) {
    AXUIElementSetAttributeValue(n.element, kAXSelectedTextRangeAttribute as CFString, range)
  }
  usleep(100_000)
  typeText(args[4], pid: pid)
case "wait":
  guard args.count >= 4 else { fail("ax: wait needs a selector") }
  let deadline = Date().addingTimeInterval(Double(args.count >= 5 ? args[4] : "60") ?? 60)
  while Date() < deadline {
    if let n = find(app, args[3]) { print(json(n)); exit(0) }
    usleep(500_000)
  }
  fail("ax: timed out waiting for \(args[3])")
default:
  fail("ax: unknown command \(command)")
}
