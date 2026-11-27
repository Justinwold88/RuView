- **Docs: README claims match the code (issue #54).** The smart-home badges
  no longer say "Works with Matter" (the bridge can't be started yet) or imply
  Google Home and Alexa work without Home Assistant. The entity summary now
  gives the real default (12 per node; heart rate, possible distress, fall and
  fall risk are opt-in) instead of 21. Heart rate is labelled experimental,
  since SYNTHETIC tests put it at chance. The fixed "1463 tests" and "10M+
  downloads" badges are replaced by live CI and crates.io badges. The Home
  Assistant guide's privacy-mode entity counts are corrected for the link
  score sensor (11 by default, not 10).
