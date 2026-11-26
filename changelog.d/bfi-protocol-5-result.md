- **Public BFI test, protocol 5 result: no better than chance on held-out
  data (MEASURED on public data).** Run 45732394943 admitted the benchmark
  and scored the never-used day-5 groups once, as ADR-430 froze it. The BFI
  motion feature was rescaled by each session's own median and fitted on
  train + validation. On the same-device held-out view it scored balanced
  accuracy 0.479 (95% interval 0.376–0.578), so it is not better than chance.
  The timing-only control scored 0.520. Normalization removed the level shift
  between sessions, but little signal remained: reading a book (labelled
  still) scored as high as jogging in every group. This closes the public BFI
  motion study on this dataset. Details in
  `tools/bfi_learning/PUBLIC-BFI-MOTION-5.md`.
