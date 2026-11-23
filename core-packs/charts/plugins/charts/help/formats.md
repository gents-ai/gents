`format` (values and the y axis), `y_format` (the y axis; overrides `format`),
`y2_format` (the right axis) and `x_format` take a number format: `,.2f`
(thousands separators, two decimals), `.1%` (percent), `~s` (1.5k, 2M), `d`
(whole numbers), `e` (scientific), with optional text around the spec in
braces: `${,.0f}`, `{.1%}`, `{~s} units`; a symbol directly around the spec
needs no braces (`$,.0f`, `.1f%`). Exact halves round away from zero (2.5 is
3, -2.5 is -3), and the minus sign comes before the symbol (`-$1,234`).

A date column (`2024-01-31`, `2024-01-31T10:00:00Z`, with offsets) becomes a
time axis with calendar ticks. Years like 2019 stay plain numbers, written
without a separator. On a time axis `x_format` is a date pattern built from
`%Y %m %d %H %M %S %b %B`, such as `%b %Y`.
