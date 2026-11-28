- **Dashboard: the Sensing tab works without internet access.** The tab
  loads its 3D view library (Three.js) from a CDN and used to wait for it
  before connecting to live data. When the CDN couldn't be reached (an
  offline or firewalled home network), the tab showed no readings, node
  status or link scores at all. It now connects first, and without Three.js
  it shows live data with a note that only the 3D view is unavailable.
