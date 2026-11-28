- **ADR-431 (proposed): how to choose the Matter SDK (issue #50).** Before
  committing to an SDK, a time-boxed spike would add rs-matter (0.3.0,
  Matter 1.6, Apache-2.0, not certified) behind the `matter` feature with
  one occupancy endpoint per room. Pass means logs showing it commissioned
  by chip-tool or Home Assistant, by an Echo Show on an Amazon developer
  account, and a second controller. Fallbacks are a Matterbridge plugin, or
  Alexa staying on the Home Assistant route. It also corrects ADR-115's note
  on rs-matter's licence.
