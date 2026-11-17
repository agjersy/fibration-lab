# Overflow measurement accuracy (W7 evidence)

`tests/test_overflow_accuracy.py` renders 40 body-text cases on the `wide-dense` test template (no
autofit, Liberation Sans 16 pt): 20 with 12 to 31 single-line paragraphs and 20 with 2 to 21 wrapping
paragraphs. LibreOffice lays each slide out; the test finds the lowest row of ink inside the body box
on the rasterized page and calls it an overflow when ink extends more than 2 px (at 144 dpi) past the
box bottom. That verdict is compared with the checker's `fit.overflow`.

Result on LibreOffice 24.2 (Linux): **39 of 40 agree.** The one miss is a boundary case where the text
runs 4 px past the box and the checker says it fits. The machine-readable result of the latest run is
`overflow-accuracy.json` next to this file.

Not yet measured: agreement with PowerPoint itself (needs a machine with PowerPoint), fonts other than
Liberation Sans, CJK text, and tables. Until then `fit.overflow` can be lowered to `warn` per project.
