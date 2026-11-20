# LiteLLM Remote design language

One canvas, one surface, one accent used exactly twice per screen, three radii,
five type roles, and a 4-point grid. Every number in the apps comes from this
file; if a value is not a token here, it does not ship. The iOS tokens live in
`ios/LiteLLMRemote/LiteLLMRemoteApp.swift` (`Palette`, `Space`, `Radius`,
`Fonts`); Android mirrors them in `Theme.kt`.

## Rules

1. **Two backgrounds only.** `canvas` behind everything and one elevated
   `surface` for bubbles, cards, and the composer. Depth inside a surface comes
   from `raised`, never a third background or a shadow.
2. **One accent, two jobs.** The user's bubble and the primary action (send /
   allow). Status colors (`good`, `warn`) are semantic and never decorate.
3. **Three radii.** `inset = 12` inside a surface, `container = 24` for bubbles
   and cards (continuous corners), capsule/circle for chips, the composer at
   rest, and icon buttons. Nothing in between.
4. **The 4-point grid decides every distance.** `s1 4 · s2 8 · s3 12 · s4 16 ·
   s5 20 · s6 24 · s8 32 · s12 48`. Gutter `s4`, bubble padding `s3/s4`,
   message gap `s3`, card padding `s4`.
5. **Five type roles, platform-native sizing.** title `.headline`, body
   `.body`, label `.subheadline.medium`, caption `.footnote`, mono
   `.footnote.monospaced` (plus `eyebrow` for tracked uppercase tags). Android:
   titleMedium / bodyLarge / labelLarge / bodySmall / bodySmall+mono. No
   literal font sizes in view code.
6. **Hairlines, not shadows.** Surfaces separate from canvas with a 1px
   `border`.

## Color roles (Display P3)

| Role | Light | Dark | Used for |
| --- | --- | --- | --- |
| canvas | #F7F8FB | #0B0D12 | screen background |
| surface | #FFFFFF | #171A21 | bubbles, cards, composer, chips |
| raised | #EEF0F5 | #232835 | chips and insets inside a surface |
| border | #E3E6EC | #2B303C | 1px hairlines |
| ink | #11182B | #ECEEF4 | primary text |
| inkSoft | #5D6574 | #9BA1AE | meta, placeholders, mono activity |
| accent | #0117BE | #5A68FF | user bubble + primary action only |
| onAccent | #FFFFFF | #FFFFFF | text/icons on accent |
| good | #178044 | #4ECB7E | ready/online dots |
| warn | #A6570F | #F2A65A | approvals eyebrow, errors |

## Component contracts

- **Bubble**: container radius, padding s3/s4, max-width ~85%. Agent = surface
  + hairline + ink; user = accent + onAccent, right-aligned. Long content
  truncates at 8,000 chars behind "Show full message"; copy via long-press.
- **Tool activity**: raised, inset radius, mono, one line `Tool · target`,
  chevron to expand.
- **Approval card**: surface, container radius, warn eyebrow, payload on a
  canvas inset (inset radius, mono, scrolls past 240pt), then a full-width
  pill pair — raised Decline, accent Allow, 44pt.
- **Composer, two states.** Resting: surface capsule, one row — 40pt raised
  "+" circle, placeholder, 40pt accent circle. Focused: expands to container
  radius, field alone on top (grows to 6 lines), control row below — "+",
  raised model chip (`model` label, tap opens the picker), spacer, accent
  circle. The accent circle is Send (↑) when idle and Stop (⏹) while the turn
  runs; never a separate stop control, never in the header. Attached files
  render as removable capsule chips above the field and travel inline as
  `<attached-file name="…">` blocks (text files ≤ 24,000 chars, five per
  message).
- **Header**: capsule chips, 40pt. Center chip = 6pt status dot (accent
  working, warn needs-attention, good ready) + session title + ⌄, opening the
  session sheet (full model picker, agent, project, status). The composer chip
  is the quick model switch; the sheet is the full list; never a third place.
- **States**: working = accent dot + caption line, never a modal; errors =
  warn on warn-at-7% inset; timers use tabular numerals.
