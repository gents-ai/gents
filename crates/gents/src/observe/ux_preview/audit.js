// Evaluated in a plugin's sandbox frame by `gents ux preview` once the page
// has rendered; the value it resolves to is `report::Audit`. It only reads.
(async () => {
  const MAX_ELEMENTS = 6000;
  const MAX_FINDINGS = 40;
  const MAX_LINES = 200;
  const MAX_LINE_CHARS = 240;
  const MAX_SAMPLE = 100;

  const settled = await new Promise((resolve) => {
    let quiet;
    const finish = (value) => {
      observer.disconnect();
      clearTimeout(quiet);
      clearTimeout(limit);
      resolve(value);
    };
    const arm = () => {
      clearTimeout(quiet);
      quiet = setTimeout(() => finish(true), 250);
    };
    const observer = new MutationObserver(arm);
    const limit = setTimeout(() => finish(false), 4000);
    observer.observe(document.documentElement, {
      subtree: true,
      childList: true,
      characterData: true,
      attributes: true,
    });
    arm();
  });

  const root = document.getElementById("root") ?? document.body;
  const viewportWidth = window.innerWidth;
  const viewportHeight = window.innerHeight;
  const viewportRoots = new Set([document.documentElement, document.body, root]);

  const styles = new Map();
  const styleOf = (el) => {
    let style = styles.get(el);
    if (!style) {
      style = getComputedStyle(el);
      styles.set(el, style);
    }
    return style;
  };
  const opacityOf = (el) => {
    let opacity = 1;
    for (let node = el; node; node = node.parentElement) {
      opacity *= parseFloat(styleOf(node).opacity);
    }
    return opacity;
  };
  const visible = (el) => {
    const style = styleOf(el);
    return (
      style.display !== "none" &&
      style.visibility !== "hidden" &&
      style.visibility !== "collapse" &&
      el.getClientRects().length > 0 &&
      opacityOf(el) > 0.01
    );
  };
  const collapse = (text) => text.replace(/\s+/g, " ").trim();
  const labelOf = (el) => {
    const id = el.id ? `#${el.id}` : "";
    const classes = Array.from(el.classList)
      .slice(0, 2)
      .map((name) => `.${name}`)
      .join("");
    return `${el.tagName.toLowerCase()}${id}${classes}`;
  };
  const pathOf = (el) => {
    const parts = [];
    for (let node = el; node && node !== root && parts.length < 3; node = node.parentElement) {
      parts.unshift(labelOf(node));
    }
    return parts.join(" > ");
  };
  const sampleOf = (el) =>
    collapse(el.innerText || el.textContent || el.getAttribute("aria-label") || "").slice(
      0,
      MAX_SAMPLE,
    );

  const elements = [];
  const walker = document.createTreeWalker(root, NodeFilter.SHOW_ELEMENT);
  for (let node = root; node && elements.length < MAX_ELEMENTS; node = walker.nextNode()) {
    elements.push(node);
  }
  const elementsTruncated = walker.nextNode() !== null;

  /* ---- visible text, block by block, in document order ---- */

  const isBlock = (el) => {
    const display = styleOf(el).display;
    return !display.startsWith("inline") && display !== "contents";
  };
  const blockOf = (el) => {
    let node = el;
    while (node && node !== root && !isBlock(node)) node = node.parentElement;
    return node ?? root;
  };
  const offscreen = (el) => {
    const rect = el.getBoundingClientRect();
    if (
      rect.top >= viewportHeight ||
      rect.left >= viewportWidth ||
      rect.bottom <= 0 ||
      rect.right <= 0
    ) {
      return true;
    }
    for (let node = el.parentElement; node && node !== root; node = node.parentElement) {
      const style = styleOf(node);
      if (style.overflowX === "visible" && style.overflowY === "visible") continue;
      const box = node.getBoundingClientRect();
      if (
        rect.top >= box.bottom ||
        rect.left >= box.right ||
        rect.bottom <= box.top ||
        rect.right <= box.left
      ) {
        return true;
      }
    }
    return false;
  };
  const blocks = new Map();
  const addText = (el, text) => {
    const value = collapse(text);
    if (!value) return;
    const block = blockOf(el);
    const parts = blocks.get(block) ?? [];
    parts.push(value);
    blocks.set(block, parts);
  };
  const textWalker = document.createTreeWalker(
    root,
    NodeFilter.SHOW_ELEMENT | NodeFilter.SHOW_TEXT,
  );
  for (let node = textWalker.nextNode(); node; node = textWalker.nextNode()) {
    if (node.nodeType === Node.TEXT_NODE) {
      if (visible(node.parentElement)) addText(node.parentElement, node.textContent);
      continue;
    }
    if (!visible(node)) continue;
    const tag = node.tagName;
    if (tag === "INPUT" || tag === "TEXTAREA" || tag === "SELECT") {
      const shown = node.value || node.getAttribute("placeholder") || "";
      if (shown) addText(node, `[${tag.toLowerCase()}: ${shown}]`);
    } else if (tag === "IMG" && node.getAttribute("alt")) {
      addText(node, `[image: ${node.getAttribute("alt")}]`);
    } else if (node.getAttribute("aria-label") && !collapse(node.textContent)) {
      addText(node, `[${node.getAttribute("aria-label")}]`);
    }
  }
  let textTruncated = blocks.size > MAX_LINES;
  const text = [];
  for (const [block, parts] of blocks) {
    if (text.length >= MAX_LINES) break;
    const line = parts.join(" ");
    if (line.length > MAX_LINE_CHARS) textTruncated = true;
    text.push({ text: line.slice(0, MAX_LINE_CHARS), offscreen: offscreen(block) });
  }

  /* ---- clipped, or past the viewport ---- */

  const clippedElements = [];
  let clippedOmitted = 0;
  const EDGES = ["right", "bottom", "left", "top"];
  const NO_EDGES = new Set();
  const pastEdge = (box, rect, edge) => {
    if (edge === "right") return rect.right - box.right;
    if (edge === "bottom") return rect.bottom - box.bottom;
    if (edge === "left") return box.left - rect.left;
    return box.top - rect.top;
  };
  const finding = (el, kind, edge, overflow, clippedBy) => ({
    kind,
    selector: pathOf(el),
    sample: sampleOf(el),
    edge,
    overflow_px: Math.round(overflow),
    clipped_by: clippedBy,
  });
  const clipAtEdge = (el, rect, edge) => {
    const horizontal = edge === "right" || edge === "left";
    let position = styleOf(el).position;
    for (let node = el.parentElement; node && position !== "fixed"; node = node.parentElement) {
      const style = styleOf(node);
      if (position === "absolute" && style.position === "static") continue;
      position = style.position;
      const mode = horizontal ? style.overflowX : style.overflowY;
      if (mode === "auto" || mode === "scroll") {
        /* an inner scroller reaches this; a document that scrolls sideways never reads right */
        if (horizontal && viewportRoots.has(node)) break;
        return null;
      }
      if (mode === "visible") continue;
      const overflow = pastEdge(node.getBoundingClientRect(), rect, edge);
      if (overflow > 1) {
        return viewportRoots.has(node)
          ? finding(el, "past_viewport", edge, overflow, null)
          : finding(el, "clipped", edge, overflow, labelOf(node));
      }
    }
    if (edge === "bottom") return null;
    const viewport = { left: 0, top: 0, right: viewportWidth, bottom: viewportHeight };
    const overflow = pastEdge(viewport, rect, edge);
    return overflow > 1 ? finding(el, "past_viewport", edge, overflow, null) : null;
  };
  const record = (item) => {
    if (clippedElements.length < MAX_FINDINGS) clippedElements.push(item);
    else clippedOmitted += 1;
  };
  /* an element past an edge carries its descendants past that edge too */
  const edgesPast = new Map();
  for (const el of elements) {
    const inherited = edgesPast.get(el.parentElement) ?? NO_EDGES;
    edgesPast.set(el, inherited);
    if (viewportRoots.has(el) || !visible(el)) continue;
    const rect = el.getBoundingClientRect();
    if (rect.width <= 1 || rect.height <= 1) continue;
    let edges = inherited;
    for (const edge of EDGES) {
      if (edges.has(edge)) continue;
      const found = clipAtEdge(el, rect, edge);
      if (!found) continue;
      record(found);
      edges = new Set(edges).add(edge);
    }
    edgesPast.set(el, edges);
    const ownText = Array.from(el.childNodes).some(
      (node) => node.nodeType === Node.TEXT_NODE && node.textContent.trim(),
    );
    if (
      ownText &&
      styleOf(el).overflowX !== "visible" &&
      el.clientWidth > 0 &&
      el.scrollWidth > el.clientWidth + 1
    ) {
      record(finding(el, "text_truncated", "right", el.scrollWidth - el.clientWidth, null));
    }
  }

  /* ---- custom properties the sandbox does not define ---- */

  const VAR = /var\(\s*(--[\w-]+)\s*(,)?/g;
  const used = new Map();
  const declaredElsewhere = new Set();
  const ROOT_SCOPED = /(^|,|\s)(:root|html)\b/;
  const note = (text, where) => {
    for (const match of text.matchAll(VAR)) {
      if (!match[2] && !used.has(match[1])) used.set(match[1], where);
    }
  };
  const declare = (style, scopedToRoot) => {
    for (const property of style) {
      if (property.startsWith("--") && !scopedToRoot) declaredElsewhere.add(property);
    }
  };
  const walkRules = (rules, visit) => {
    for (const rule of rules) {
      visit(rule);
      if (rule.cssRules) walkRules(rule.cssRules, visit);
    }
  };
  for (const sheet of document.styleSheets) {
    const plugin = sheet.ownerNode?.dataset?.gentsUx !== undefined;
    walkRules(sheet.cssRules, (rule) => {
      if (rule.constructor.name === "CSSPropertyRule") declaredElsewhere.add(rule.name);
      if (!rule.style) return;
      declare(rule.style, ROOT_SCOPED.test(rule.selectorText ?? ""));
      if (plugin) note(rule.cssText, `plugin stylesheet: ${rule.selectorText}`);
    });
  }
  for (const el of elements) {
    const inline = el.getAttribute("style");
    if (!inline) continue;
    declare(el.style, false);
    note(inline, `inline style on ${pathOf(el)}`);
  }
  const rootComputed = getComputedStyle(document.documentElement);
  const undefinedTokens = [];
  for (const [name, where] of used) {
    if (rootComputed.getPropertyValue(name) !== "" || declaredElsewhere.has(name)) continue;
    undefinedTokens.push({ name, used_in: where });
  }
  const undefinedOmitted = Math.max(0, undefinedTokens.length - MAX_FINDINGS);

  /* ---- low contrast ---- */

  const paint = document.createElement("canvas").getContext("2d", { willReadFrequently: true });
  const colors = new Map();
  const colorOf = (css) => {
    let color = colors.get(css);
    if (!color) {
      paint.clearRect(0, 0, 1, 1);
      paint.fillStyle = css;
      paint.fillRect(0, 0, 1, 1);
      const [r, g, b, a] = paint.getImageData(0, 0, 1, 1).data;
      color = { r, g, b, a: a / 255 };
      colors.set(css, color);
    }
    return color;
  };
  const over = (top, under) => ({
    r: top.r * top.a + under.r * (1 - top.a),
    g: top.g * top.a + under.g * (1 - top.a),
    b: top.b * top.a + under.b * (1 - top.a),
    a: 1,
  });
  const backgroundOf = (el) => {
    const layers = [];
    for (let node = el; node; node = node.parentElement) {
      const style = styleOf(node);
      if (style.backgroundImage !== "none") return null;
      const color = colorOf(style.backgroundColor);
      if (color.a === 0) continue;
      layers.push(color);
      if (color.a === 1) break;
    }
    let base = { r: 255, g: 255, b: 255, a: 1 };
    for (const layer of layers.reverse()) base = over(layer, base);
    return base;
  };
  const luminance = ({ r, g, b }) => {
    const channel = (value) => {
      const scaled = value / 255;
      return scaled <= 0.03928 ? scaled / 12.92 : ((scaled + 0.055) / 1.055) ** 2.4;
    };
    return 0.2126 * channel(r) + 0.7152 * channel(g) + 0.0722 * channel(b);
  };
  const css = ({ r, g, b }) => `rgb(${Math.round(r)}, ${Math.round(g)}, ${Math.round(b)})`;
  const contrastGroups = new Map();
  let lowContrastSkipped = 0;
  for (const el of elements) {
    const own = collapse(
      Array.from(el.childNodes)
        .filter((node) => node.nodeType === Node.TEXT_NODE)
        .map((node) => node.textContent)
        .join(" "),
    );
    if (!own || !visible(el) || el.closest("[disabled], [aria-disabled='true']")) continue;
    const background = backgroundOf(el);
    if (!background) {
      lowContrastSkipped += 1;
      continue;
    }
    const style = styleOf(el);
    const foreground = colorOf(style.color);
    const text = over({ ...foreground, a: foreground.a * opacityOf(el) }, background);
    const lighter = Math.max(luminance(text), luminance(background));
    const darker = Math.min(luminance(text), luminance(background));
    const ratio = (lighter + 0.05) / (darker + 0.05);
    const size = parseFloat(style.fontSize);
    const large = size >= 24 || (size >= 18.66 && parseInt(style.fontWeight, 10) >= 700);
    const required = large ? 3 : 4.5;
    if (ratio >= required) continue;
    const key = `${css(text)}|${css(background)}|${required}`;
    const group = contrastGroups.get(key);
    if (group) {
      group.count += 1;
    } else {
      contrastGroups.set(key, {
        ratio: Math.round(ratio * 100) / 100,
        required,
        color: css(text),
        background: css(background),
        selector: pathOf(el),
        sample: own.slice(0, MAX_SAMPLE),
        count: 1,
      });
    }
  }
  const lowContrast = Array.from(contrastGroups.values())
    .sort((a, b) => a.ratio - b.ratio)
    .slice(0, MAX_FINDINGS);

  return {
    settled,
    viewport: { width: viewportWidth, height: viewportHeight },
    root_scroll: { width: root.scrollWidth, height: root.scrollHeight },
    text,
    text_truncated: textTruncated,
    clipped: clippedElements,
    clipped_omitted: clippedOmitted,
    undefined_tokens: undefinedTokens.slice(0, MAX_FINDINGS),
    undefined_tokens_omitted: undefinedOmitted,
    low_contrast: lowContrast,
    low_contrast_omitted: Math.max(0, contrastGroups.size - MAX_FINDINGS),
    contrast_skipped_over_images: lowContrastSkipped,
    elements_truncated: elementsTruncated,
  };
})()
