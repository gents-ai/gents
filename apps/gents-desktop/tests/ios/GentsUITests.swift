import XCTest
import UIKit
import Vision

final class GentsUITests: XCTestCase {
    private struct VisualText {
        let value: String
        let bounds: CGRect
    }

    private enum VisualTestError: Error {
        case missingText(String)
    }

    private var app: XCUIApplication!

    override func setUpWithError() throws {
        continueAfterFailure = false
        app = XCUIApplication()
        app.launchEnvironment["RUST_BACKTRACE"] = "full"
        app.launchEnvironment["RUST_LOG"] = "info"
        app.launch()
        XCTAssertTrue(
            waitUntil(timeout: 20) { self.app.state == .runningForeground },
            "Gents did not reach the foreground"
        )
        // Asking XCTest for the WKWebView hierarchy while WebKit is still
        // attaching can deadlock its snapshot request for the full timeout.
        // Let the native shell finish first-frame setup before the first query.
        RunLoop.current.run(until: Date().addingTimeInterval(8))
    }

    override func tearDownWithError() throws {
        if testRun?.hasSucceeded == false {
            let attachment = XCTAttachment(screenshot: XCUIScreen.main.screenshot())
            attachment.name = "failure"
            attachment.lifetime = .keepAlways
            add(attachment)

            let transcript = ((try? recognizedText()) ?? [])
                .map(\.value)
                .joined(separator: "\n")
            let visibleText = XCTAttachment(string: transcript)
            visibleText.name = "visible-text"
            visibleText.lifetime = .keepAlways
            add(visibleText)
        }
    }

    func testAmyPromptRoundTrip() throws {
        let environment = ProcessInfo.processInfo.environment
        let marker = environment["GENTS_E2E_EXPECTED_RESPONSE"]
            ?? "AMY_IPHONE_SIMULATOR_E2E"
        let prompt = environment["GENTS_E2E_PROMPT"]
            ?? "Reply with only the uppercase underscore form of: amy iphone simulator e2e."

        _ = try waitForVisualText("Fleet Dashboard", timeout: 30)

        var amy = try findVisualText("Amy", exact: true)
        if amy == nil {
            try pairAmy(invite: pairingInvite(from: environment))
            amy = try waitForVisualText("Amy", exact: true, timeout: 120)
        }
        tap(try XCTUnwrap(amy, "Amy did not appear after pairing"))

        let composer = try waitForVisualText("Message the selected agent", timeout: 30)
        tap(composer)
        paste(prompt)
        app.typeKey(XCUIKeyboardKey.return.rawValue, modifierFlags: [])

        _ = try waitForVisualText(marker, timeout: 120)

        let attachment = XCTAttachment(screenshot: XCUIScreen.main.screenshot())
        attachment.name = "amy-round-trip"
        attachment.lifetime = .keepAlways
        add(attachment)
    }

    private func pairAmy(invite: String) throws {
        if let disclosure = try findVisualText("Connect a remote agent") {
            tap(disclosure)
        } else {
            tap(try waitForVisualText("Add Agent", timeout: 10))
        }

        let label = try scrollUntilVisualText("Agent label", timeout: 30)
        tapBelow(label)
        paste("Amy")
        dismissKeyboard()

        let token = try scrollUntilVisualText("Pairing invite", timeout: 30)
        tapBelow(token)
        paste(invite)
        dismissKeyboard()

        tap(try scrollUntilVisualText("Pair securely", timeout: 30))
    }

    private func pairingInvite(from environment: [String: String]) throws -> String {
        if let value = environment["GENTS_E2E_PAIR_TOKEN"], !value.isEmpty {
            return value
        }
        if let value = UIPasteboard.general.string, value.hasPrefix("dabear1-") {
            return value
        }
        throw XCTSkip(
            "Set GENTS_E2E_PAIR_TOKEN or copy a fresh Amy bearer invite to the simulator pasteboard"
        )
    }

    private func recognizedText() throws -> [VisualText] {
        let screenshot = XCUIScreen.main.screenshot().image
        guard let image = screenshot.cgImage else {
            return []
        }

        let request = VNRecognizeTextRequest()
        request.recognitionLevel = .accurate
        request.usesLanguageCorrection = false
        request.minimumTextHeight = 0.012
        try VNImageRequestHandler(cgImage: image).perform([request])

        return (request.results ?? []).compactMap { observation in
            observation.topCandidates(1).first.map {
                VisualText(value: $0.string, bounds: observation.boundingBox)
            }
        }
    }

    private func findVisualText(
        _ expected: String,
        exact: Bool = false
    ) throws -> VisualText? {
        let expectedValue = normalized(expected)
        return try recognizedText().first { candidate in
            let value = normalized(candidate.value)
            return exact ? value == expectedValue : value.contains(expectedValue)
        }
    }

    private func waitForVisualText(
        _ expected: String,
        exact: Bool = false,
        timeout: TimeInterval
    ) throws -> VisualText {
        let deadline = Date().addingTimeInterval(timeout)
        while Date() < deadline {
            if let match = try findVisualText(expected, exact: exact) {
                return match
            }
            RunLoop.current.run(until: Date().addingTimeInterval(0.75))
        }
        XCTFail("Visible text \(expected.debugDescription) did not appear")
        throw VisualTestError.missingText(expected)
    }

    private func scrollUntilVisualText(
        _ expected: String,
        timeout: TimeInterval,
        exact: Bool = false
    ) throws -> VisualText {
        let deadline = Date().addingTimeInterval(timeout)
        while Date() < deadline {
            if let match = try findVisualText(expected, exact: exact) {
                return match
            }
            swipeUp()
            RunLoop.current.run(until: Date().addingTimeInterval(0.75))
        }
        XCTFail("Could not scroll to visible text \(expected.debugDescription)")
        throw VisualTestError.missingText(expected)
    }

    private func tap(_ text: VisualText) {
        let point = CGVector(
            dx: text.bounds.midX,
            dy: 1 - text.bounds.midY
        )
        app.coordinate(withNormalizedOffset: point).tap()
    }

    private func tapBelow(_ text: VisualText) {
        let point = CGVector(
            dx: 0.5,
            dy: min(0.95, 1 - text.bounds.minY + 0.045)
        )
        app.coordinate(withNormalizedOffset: point).tap()
    }

    private func paste(_ value: String) {
        UIPasteboard.general.string = value
        app.typeKey("v", modifierFlags: .command)
    }

    private func dismissKeyboard() {
        app.coordinate(withNormalizedOffset: CGVector(dx: 0.92, dy: 0.12)).tap()
        RunLoop.current.run(until: Date().addingTimeInterval(0.4))
    }

    private func swipeUp() {
        let start = app.coordinate(withNormalizedOffset: CGVector(dx: 0.85, dy: 0.82))
        let end = app.coordinate(withNormalizedOffset: CGVector(dx: 0.85, dy: 0.28))
        start.press(forDuration: 0.05, thenDragTo: end)
    }

    private func normalized(_ value: String) -> String {
        value
            .lowercased()
            .split(whereSeparator: \.isWhitespace)
            .joined(separator: " ")
    }

    private func waitUntil(
        timeout: TimeInterval,
        poll: TimeInterval = 0.2,
        condition: () -> Bool
    ) -> Bool {
        let deadline = Date().addingTimeInterval(timeout)
        while Date() < deadline {
            if condition() {
                return true
            }
            RunLoop.current.run(until: Date().addingTimeInterval(poll))
        }
        return condition()
    }
}
