// Message formatting, following what WhatsApp does, because that is what
// people already have in their fingers:
//
//   *bold*   _italic_   ~strikethrough~   `monospace`   ```block```
//
// and newlines, which are simply newlines.
//
// Parsing is kept apart from drawing on purpose. `parseMessage` returns plain
// data and nothing else, which is what `scripts/check-format.mjs` exercises;
// `renderMessage` turns that data into elements. Nothing here ever builds
// markup from a string — a message is written by somebody else, and the one
// rule this app cannot afford to break is that their text is only ever text.

/// One piece of a parsed message.
export type Span =
  | { kind: "text"; text: string }
  | { kind: "break" }
  /// Fixed-width, and never formatted inside: backticks mean "as typed".
  | { kind: "mono"; text: string }
  | { kind: "block"; text: string }
  | { kind: "bold" | "italic" | "strike"; spans: Span[] };

/// The marker characters and what they produce.
const INLINE = {
  "*": "bold",
  _: "italic",
  "~": "strike",
} as const;

type Marker = keyof typeof INLINE;

function isMarker(c: string): c is Marker {
  return c === "*" || c === "_" || c === "~";
}

/// A marker only opens when it is followed by something other than a space,
/// and only closes when it is preceded by one. That is what keeps a lone
/// asterisk in "2 * 3" from turning the rest of the line bold, and it is the
/// rule WhatsApp uses.
function isSpace(c: string | undefined): boolean {
  return c === undefined || c === " " || c === "\n" || c === "\t";
}

/// Where the run opened at `open` closes, or -1 if it never does.
///
/// The first valid closing marker wins, so `*a* b *c*` is two bold words
/// rather than one long one.
function closingIndex(text: string, open: number, marker: string): number {
  for (let i = open + marker.length; i <= text.length - marker.length; i++) {
    if (!text.startsWith(marker, i)) continue;
    // Nothing between the two markers is not emphasis, it is two markers.
    if (i === open + marker.length) continue;
    if (isSpace(text[i - 1])) continue;
    return i;
  }
  return -1;
}

/// Breaks `text` into spans. Anything that does not parse as formatting stays
/// exactly as it was typed, markers included: a message that does not quite
/// follow the rules should read as what the person wrote, not disappear.
export function parseMessage(text: string): Span[] {
  const spans: Span[] = [];
  let plain = "";

  const flush = () => {
    if (plain) spans.push({ kind: "text", text: plain });
    plain = "";
  };

  let i = 0;
  while (i < text.length) {
    const c = text[i];

    if (c === "\n") {
      flush();
      spans.push({ kind: "break" });
      i += 1;
      continue;
    }

    // A fenced block comes first: inside it, every other marker is literal.
    if (text.startsWith("```", i)) {
      const end = text.indexOf("```", i + 3);
      if (end > i + 3) {
        flush();
        spans.push({ kind: "block", text: text.slice(i + 3, end) });
        i = end + 3;
        continue;
      }
    }

    if (c === "`") {
      const end = closingIndex(text, i, "`");
      if (end !== -1 && !isSpace(text[i + 1])) {
        flush();
        spans.push({ kind: "mono", text: text.slice(i + 1, end) });
        i = end + 1;
        continue;
      }
    }

    if (isMarker(c) && !isSpace(text[i + 1])) {
      const end = closingIndex(text, i, c);
      if (end !== -1) {
        flush();
        spans.push({
          kind: INLINE[c],
          // Emphasis nests: *_both_* is bold and italic.
          spans: parseMessage(text.slice(i + 1, end)),
        });
        i = end + 1;
        continue;
      }
    }

    plain += c;
    i += 1;
  }

  flush();
  return spans;
}

/// The message with its markers removed, for places that show a line of text
/// rather than a formatted message — a notification, mostly.
export function plainText(text: string): string {
  const walk = (spans: Span[]): string =>
    spans
      .map((span) => {
        switch (span.kind) {
          case "text":
            return span.text;
          case "break":
            return " ";
          case "mono":
          case "block":
            return span.text;
          default:
            return walk(span.spans);
        }
      })
      .join("");
  return walk(parseMessage(text));
}

/// Builds the elements for `text`, ready to be appended to a bubble.
export function renderMessage(text: string): DocumentFragment {
  const fragment = document.createDocumentFragment();

  const draw = (spans: Span[], into: Node) => {
    for (const span of spans) {
      switch (span.kind) {
        case "text":
          into.appendChild(document.createTextNode(span.text));
          break;
        case "break":
          into.appendChild(document.createElement("br"));
          break;
        case "mono": {
          const code = document.createElement("code");
          code.textContent = span.text;
          into.appendChild(code);
          break;
        }
        case "block": {
          const pre = document.createElement("pre");
          pre.textContent = span.text;
          into.appendChild(pre);
          break;
        }
        default: {
          const tag = { bold: "strong", italic: "em", strike: "s" }[span.kind];
          const element = document.createElement(tag);
          draw(span.spans, element);
          into.appendChild(element);
        }
      }
    }
  };

  draw(parseMessage(text), fragment);
  return fragment;
}
