#!/usr/bin/env bash
# Lab: click YARD's drawn window controls on a real X11 window and record what happened.
set -u
out=controls.txt; : > $out
say() { echo "$*" | tee -a $out; }
state() { xprop -id "$WID" _NET_WM_STATE 2>/dev/null | sed 's/.*= //'; }
geom() { xdotool getwindowgeometry "$WID" | tr '\n' ' '; }
sleep 1
WID=$(xdotool search --classname vn.risotech.yard | head -1)
say "window $WID"
say "decorations: $(xprop -id "$WID" _GTK_FRAME_EXTENTS _MOTIF_WM_HINTS 2>&1 | tr '\n' ' ')"
eval $(xdotool getwindowgeometry --shell "$WID")
say "start: $(geom) state=[$(state)]"
click() { xdotool mousemove --window "$WID" "$1" "$2" click --repeat "${3:-1}" --delay 120 1; sleep 1.5; }
# Maximize, then restore, with the buttons (positions from the right edge).
click $((WIDTH-66)) 27; say "after maximize button: state=[$(state)] $(geom)"
eval $(xdotool getwindowgeometry --shell "$WID")
click $((WIDTH-55)) 16; say "after restore button: state=[$(state)] $(geom)"
eval $(xdotool getwindowgeometry --shell "$WID")
# Double-click the title bar, twice.
click $((WIDTH/2)) 27 2; say "after double-click: state=[$(state)]"
eval $(xdotool getwindowgeometry --shell "$WID")
click $((WIDTH/2)) 16 2; say "after second double-click: state=[$(state)]"
eval $(xdotool getwindowgeometry --shell "$WID")
# Move by dragging the title bar.
before="$(geom)"
xdotool mousemove --window "$WID" 300 27 mousedown 1 sleep 0.3 mousemove_relative --sync 120 80 sleep 0.3 mouseup 1; sleep 1.5
say "move: before [$before] after [$(geom)]"
eval $(xdotool getwindowgeometry --shell "$WID")
# Resize by dragging the right edge.
before="$(geom)"
xdotool mousemove --window "$WID" $((WIDTH-6)) $((HEIGHT/2)) mousedown 1 sleep 0.3 mousemove_relative --sync 100 0 sleep 0.3 mouseup 1; sleep 1.5
say "resize right edge: before [$before] after [$(geom)]"
eval $(xdotool getwindowgeometry --shell "$WID")
# Window menu on right-click; dismiss it.
xdotool mousemove --window "$WID" 300 27 click 3; sleep 1; say "right-click done"; xdotool key Escape; sleep 1
# Minimize, then bring back.
click $((WIDTH-98)) 27; say "after minimize button: state=[$(state)]"
xdotool windowactivate --sync "$WID" 2>/dev/null || xdotool windowmap "$WID"; sleep 1.5
say "after activate: state=[$(state)]"
eval $(xdotool getwindowgeometry --shell "$WID")
# Close.
click $((WIDTH-34)) 27; sleep 1
if pgrep -f target/debug/yard >/dev/null; then say "after close button: still running"; else say "after close button: app exited"; fi
