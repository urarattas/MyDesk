# TODO

## Input

- [ ] **Wheel scrolling on Wayland.** ydotool 1.0.4 only exposes
      `click / key / mousemove / type`, so the ydotool backend cannot scroll.
      Either add a `dotool` backend (it has `scroll`) or talk to `/dev/uinput`
      directly and emit `REL_WHEEL`; the latter also drops the ydotool
      dependency and is the preferred route.
- [ ] Multi-monitor mapping: coordinates are normalised to the primary display,
      so sharing a secondary screen is offset.
- [ ] Wayland absolute pointer uses the DRM preferred mode; querying the output
      the browser is actually capturing would handle mixed-resolution setups.

## Audio

- [ ] Tell the user when the shared source has no audio track (Chrome only
      offers tab/system audio for some capture sources) instead of leaving the
      speaker button disabled without explanation.

## Deployment

- [ ] Publish `MyDeskBridge.exe` from CI. The workflow already refreshes the
      rolling `bridge-latest` release on pushes to `server`; it needs a push to
      run.
