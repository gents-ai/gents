CSV types are inferred: integers stay integers (a number with a leading zero, such as a zip code, stays text),
a column becomes float only when a float is in it, `YYYY-MM-DD` is a date, `YYYY-MM-DD[T ]HH:MM[:SS[.f]]` a
timestamp, `true` and `false` booleans. A first row is a header when all its filled cells are text and either no
cell is empty and the names are distinct, or a column below holds numbers, dates or booleans; so `,a,b` over `0,1,2`
(a pandas index column) is a header whose empty cell is named `column_1`, and a table has at most 2000 columns.
An empty field is NULL; an empty quoted field is the empty string. Types come from the first 100000 rows; a later
value that disagrees makes the call read the whole table once to settle the type; when such a table's result has
more than one page, the first page already reads it once so that every page has the same column types. Other date
formats are text: use `to_date(col, '%d/%m/%Y')`. `delimiter` (one character or `tab`) and `header` (true or
false) override the detection.
