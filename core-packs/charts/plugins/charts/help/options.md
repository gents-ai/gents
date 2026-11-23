`stack` (`grouped` or `stacked`) and `horizontal` (`true`) for bar charts;
`line_axis` (`right` or `left`) for combo charts; `title`, `subtitle`;
`x_label`, `y_label`, `y2_label` (default: the column names; an empty string
removes one); `x_scale` (`auto`, `linear`, `log`, `time`, `category`),
`y_scale` and `y2_scale` (`linear`, `log`); `x_min`, `x_max`, `y_min`, `y_max`
to fix the visible range (values outside it are cut off and counted in
`warnings`); `legend` (`auto`, `right`, `bottom`, `top`, `left`, `none`);
`theme` (`light`, `dark`); `colors` (hex list); `width` (200 to 4096) and
`height` (150 to 4096), default 800 by 480; `scale` (PNG pixel density, 0.5 to
4); `output` (`both`, `svg`, `png`). Number and date formats are in the
formats topic.

- A heatmap's colour bar is on the right whatever `legend` says (`none` removes
  it); an empty cell is a dashed outline with a diagonal, never a colour.
  `center` sets the value drawn as the neutral colour of a diverging scale.
- A title is left out, with a warning, when the picture is too small for it.
- A log axis needs values above zero; a zero or negative value is an error
  that names the first one.
- Pick colours with `colors` only when asked; the default palette stays
  distinguishable for colour-blind readers, in light and dark.
- One image is at most 16 million pixels (`width` x `height` x `scale`
  squared); a larger request is refused with that number.
