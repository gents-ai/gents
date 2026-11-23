| `chart` | Draws | Notes |
| --- | --- | --- |
| `line` | lines through numbers, dates or categories | x may be numbers, ISO dates or text |
| `area` | a filled line from zero | |
| `stacked_area` | areas piled on each other | a series with no value at an x counts as zero there, and says so |
| `bar` | grouped bars; `stack: "stacked"` stacks them, `horizontal: true` lays them down | value labels on up to 30 bars |
| `stacked_bar`, `horizontal_bar` | shorthands for the two options | |
| `scatter` | points; `series` colours groups | each group's correlation is in the result |
| `bubble` | points sized by `size` | bubble area is proportional to the value, from zero; sizes of zero or below are not drawn and are counted |
| `histogram` | counts of one numeric column; `bins` is a number (1 to 200) or `auto` | `series` overlays groups on shared bins |
| `box` | box and whisker per `x` group, or per value column | whiskers reach 1.5 interquartile ranges, outliers are dots |
| `pie`, `donut` | shares of a whole | more than 12 slices: the smallest become "Other" |
| `heatmap` | a colour grid over `x` and `y` categories, coloured by `value` | sequential colours, or diverging when values cross zero or `center` is set |
| `combo` | bars (`y`) with lines (`line`) on a second axis | `line_axis: "left"` puts the lines on the first axis |
