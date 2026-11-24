#!/usr/bin/env bash
# Fail when a line of a proof has no plain-words comment above it.
#
#   proofs/check-comments.sh            every .tla and MC_*.cfg under proofs/tla and
#                                       every .lean under proofs/lean
#   proofs/check-comments.sh --except REGEX
#                                       the same, leaving out the files whose
#                                       path matches REGEX
#   proofs/check-comments.sh FILE...    only these files
#
# The rule. Every line that is not blank and not a comment must have a comment
# line directly above it (no blank line between), unless it only continues the
# statement of the line directly above it. A line continues the line above when
# one of these holds, and nothing else counts:
#   1. a bracket opened earlier in the statement is still open: ( [ { <<, and
#      in Lean also the angle brackets of an anonymous constructor;
#   2. the line above ends with an operator that still needs its right-hand
#      side (a comma, =, a set or arithmetic or logic operator, an arrow);
#      but each name of a CONSTANTS or VARIABLES list needs its own comment,
#      comma or not, because each one is a thing of its own to explain;
#   3. the line starts with such an operator or with a closing bracket.
# A new conjunct (/\), a new disjunct (\/), a THEN or ELSE branch, a tactic, a
# case arm, a bullet: each of these starts a thought of its own and needs its
# own comment. A comment at the end of a code line does not count: the comment
# goes above the line it explains.
#
# Not checked, because they carry no meaning of their own: the `---- MODULE`
# header, the `----` rules between sections and the closing `====`.
#
# Comment lines are `\*` and `(* ... *)` in TLA+ and its configurations, and
# `--` and `/- ... -/` (doc comments included) in Lean.
set -euo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")"

except='^$'
if [ "${1:-}" = "--except" ]; then
  except="$2"
  shift 2
fi
if [ "$#" -eq 0 ]; then
  # Every proof file, in a fixed order. Lake's build directory is not ours.
  files=()
  while IFS= read -r f; do files+=("$f"); done < <(
    {
      find tla -maxdepth 1 \( -name '*.tla' -o -name 'MC_*.cfg' \) | sort
      find lean -name '*.lean' -not -path '*/.lake/*' | sort
    } | grep -v -E -- "$except"
  )
else
  # The caller's paths are relative to where it stood, not to proofs/.
  cd "$OLDPWD"
  files=("$@")
fi

awk '
# Count how often the regular expression re matches in s.
function count(s, re,    n) { n = 0; while (match(s, re)) { n++; s = substr(s, RSTART + RLENGTH) } return n }

# The line with string literals and comments taken out and blanks trimmed.
function bare(s) {
  # A string literal can hold anything, a bracket included.
  gsub(/"[^"]*"/, "\"\"", s)
  if (lean) {
    # A block comment that opens and closes on this line, then a line comment.
    gsub(/\/-.*-\//, "", s)
    sub(/--.*$/, "", s)
  } else {
    gsub(/\(\*.*\*\)/, "", s)
    sub(/\\\*.*$/, "", s)
  }
  sub(/^[ \t]+/, "", s)
  sub(/[ \t]+$/, "", s)
  return s
}

# How many brackets the line opens, less how many it closes.
function opened(s,    d) {
  d = count(s, "[(\\[{]") - count(s, "[)\\]}]")
  if (lean) d += count(s, "⟨") - count(s, "⟩")
  else d += count(s, "<<") - count(s, ">>")
  return d
}

# Whether the line ends with an operator that needs a right-hand side.
function open_end(s) {
  if (lean) return s ~ /(,|=|≠|\+|\*|∧|∨|→|↔|≤|≥|<|>|∈|∉|⊆|∘|×|\+\+|::|¬)$/ && s !~ /(=>|:=|<;>| at \*)$/
  return s ~ /(,|=|#|\+|\*|\\cup|\\cap|\\union|\\o|\\in|\\notin|\\subseteq|\\X|->|=>|\/\\|\\\/|<|>|\.\.|\\)$/ && s !~ /(==|>>)$/
}

# Whether the line starts with such an operator or with a closing bracket.
function open_start(s) {
  if (lean) return s ~ /^(\)|\]|\}|⟩|,|∧|∨|→|↔|=[^>]|≠|≤|≥|\+|\*|::|∘|×|\|>|<\|)/
  return s ~ /^(\)|\]|\}|>>|,|\\cup|\\cap|\\union|\\o([^a-z]|$)|\\in([^a-z]|$)|\\notin|\\subseteq|\\X|=|<=>|#|\+|\*|->|\|->|\.\.)/ && s !~ /^==/
}

FNR == 1 {
  lean = (FILENAME ~ /\.lean$/)
  cfg = (FILENAME ~ /\.cfg$/)
  # A .tla file is free text before its module header; other files are not.
  inside = (lean || cfg)
  block = 0; depth = 0; decl = 0; above = "blank"; above_text = ""
  nfiles++
}

{
  text = $0
  sub(/^[ \t]+/, "", text)
  sub(/[ \t\r]+$/, "", text)

  # Inside a block comment: a comment line. It ends where the closer is.
  if (block > 0) {
    if (lean) block += count(text, "\\/-") - count(text, "-\\/")
    else block += count(text, "\\(\\*") - count(text, "\\*\\)")
    if (block < 0) block = 0
    above = "comment"; next
  }
  if (text == "") { above = "blank"; depth = 0; decl = 0; next }

  if (!lean && !cfg) {
    # The module header starts the module; the closing rule ends it.
    if (text ~ /^-----* *MODULE /) { inside = 1; above = "rule"; depth = 0; next }
    if (!inside) next
    if (text ~ /^=====*$/) { inside = 0; next }
    if (text ~ /^-----*$/) { above = "rule"; depth = 0; next }
  }

  if (lean) {
    if (text ~ /^--/) { above = "comment"; next }
    if (text ~ /^\/-/) {
      block = count(text, "\\/-") - count(text, "-\\/")
      if (block < 0) block = 0
      above = "comment"; next
    }
  } else {
    if (text ~ /^\\\*/) { above = "comment"; next }
    if (text ~ /^\(\*/) {
      block = count(text, "\\(\\*") - count(text, "\\*\\)")
      if (block < 0) block = 0
      above = "comment"; next
    }
  }

  # A code line.
  ncode++
  code = bare(text)
  # A CONSTANTS or VARIABLES list: every name in it is checked on its own.
  if (!lean && !cfg && code ~ /^(CONSTANTS?|VARIABLES?)([ \t]|$)/) decl = 1
  continues = (above == "code" && (depth > 0 || (!decl && open_end(above_text)) || open_start(code)))
  if (above != "comment" && !continues) {
    printf "%s:%d: no comment above: %s\n", FILENAME, FNR, text
    nbad++
  }
  # The list ends with the first name that has no comma after it.
  if (decl && code !~ /,$/ && code !~ /^(CONSTANTS?|VARIABLES?)$/) decl = 0
  depth += opened(code)
  if (depth < 0) depth = 0
  above = "code"; above_text = code
}

END {
  if (nbad > 0) {
    printf "check-comments: %d of %d code lines in %d files have no comment above them\n", nbad, ncode, nfiles
    exit 1
  }
  printf "check-comments: %d code lines in %d files, each with a comment above it\n", ncode, nfiles
}
' "${files[@]}"
