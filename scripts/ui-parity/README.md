# Window parity: boards against egui

Tools to compare the v1 window (egui) with the design boards, state by
state, at both window sizes, 1x and 2x, dark and light.

- `ui-shot` (`cargo build -p hushmic --example ui-shot`): renders a
  registered scene in a real eframe window (glow) and writes a PNG.
  `ui-shot --list` lists the scenes. Run it under Xvfb;
  `WINIT_X11_SCALE_FACTOR=2` gives 2x.
- `board-shot.mjs`: renders one state of a board (`hm-canvas/project/*.dc.html`
  in `$HM_DESIGN`, default `~/hm-v1/design`) in headless Chrome, cropped
  to the window's content. `--ticks N` steps the boards' live levels N
  times, so the columns are the first N clip frames (scenes prefill the
  same with `cols=N`). `--measure 'css;selectors'` prints element rects.
- `parity.py`: runs both for the cases in `scenes.json` and writes
  `<case>-<theme>-<size>-<scale>x-{board,egui,side,diff}.png` to
  `~/hm-v1/shots/parity/`, with the mean difference per shot.
- `zoom.py STEM X,Y,W,H`: one region enlarged (board, egui, diff) and the
  shift that would line egui up with the board.

- `a11y-tree.sh SCENE THEME WxH [key=value …]`: the scene's tree as a
  screen reader gets it over AT-SPI (role, name, states), from a private
  session bus with the accessibility bus on (`a11y-tree.py` prints it;
  needs at-spi2-core and python3-gi).

A case is one line in `scenes.json`: the board, its tweaks (`props`), the
`ticks`, the egui `scene` and its `params`, and the `sizes` it applies to.

    python3 scripts/ui-parity/parity.py --list
    python3 scripts/ui-parity/parity.py kit-main-on --sizes default --scales 1
    python3 scripts/ui-parity/zoom.py ~/hm-v1/shots/parity/kit-main-on-dark-default-1x 640,25,300,45

Live has no cases: its page (two spectrograms on one time axis, the
microphone above and what apps hear below, with the controls in one row
under them) was built after the boards, and App-Live2 still shows the
earlier path picture and its three cards. Judge `page/live` and
`paint/live` by eye. The `kit-*` cases on App-Main still cover the path
picture, which first run shows.

Files has no cases either: its figure is the same picture with the whole
file as its time axis (the original above, the cleaned sound below as far
as it is cleaned, one playhead across both), which took the place of the
reel and the time line App-Files2 still shows. Judge `page/files` by eye,
state by state (`ui-shot --list`). The `files-settings` case, Settings ›
Files, stays.

Known, accepted differences: Chrome positions text at fractional pixels
horizontally, egui at whole pixels (glyph shapes then differ by a few
levels at the edges); the lit line’s glow (the path picture) is the round-3
rules sheet's (26 px, a blurred start 4 px before the line): the 3b boards
draw it through a board-local wrapper, the older boards and the shared
design-system bundle still draw the earlier 18 px glow.
