- **Public BFI test, protocol 5 (frozen, not yet run): one held-out score
  with a session-normalized feature.** ADR-430 adds `public-bfi-motion-5`.
  It divides the BFI motion feature by each session's own median, with no
  labels, fits the threshold on train + validation, and scores the never-used
  day-5 held-out groups once. The primary result is same-device interval
  balanced accuracy with a 95% interval from resampling recordings. It counts
  as better than chance only if the lower bound is above 0.5. Protocol 4's
  validation result prompted the design; no held-out number has been seen.
  Details in `tools/bfi_learning/PUBLIC-BFI-MOTION-5.md`.
