- **Public BFI test, protocol 4 result: admitted, and the detector is at
  chance (MEASURED on public data).** Run 44975936986 admitted the benchmark
  for the 19 activities other than S: every group has at least 2 recordings
  with an eligible interval for each. The decoder oracle matched 1,872 reports
  with tshark 4.2.2. The frozen one-feature BFI motion detector, fitted on
  train (kitchen, day 1), scored balanced accuracy 0.500 on validation
  (classroom, day 3), the same as always predicting one class. Its threshold
  sat below every validation activity's median, so it called every interval
  moving. The timing-only control scored 0.521. The held-out groups were never
  scored. Details in `tools/bfi_learning/PUBLIC-BFI-MOTION-4.md`.
