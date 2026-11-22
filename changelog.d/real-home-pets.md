- **Real-home study: homes with pets.** The real-home protocol now says how
  pets affect the labels. `home_empty` requires every pet to be out of the
  home or shut in a room with no node, outside every link's sensing area. The
  new label `pets_alone` covers time when nobody is home and pets are loose.
  `labels.py` guards and totals it like `home_empty`, never as `home_empty`.
  `score_g2.py` reports its false occupied events per hour apart from G2, as
  input for plan item 3.2. The fall study keeps pets out of the room.
