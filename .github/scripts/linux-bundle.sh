#!/usr/bin/env bash
# Lab: cargo xtask bundle linux, installed for real, URLs through xdg-open.
# Needs DISPLAY with a running X server and window manager.
set -u
out=bundle.txt; : > $out
say() { echo "$*" | tee -a $out; }
C=crates/yard_build_config/tests/configs
mkdir -p bundle-logs
for c in plain spaced other; do
  echo "bundling $c at $(date +%T)"
  timeout 1200 cargo xtask bundle linux --config $C/$c/product.toml --out /tmp/dist-$c > bundle-logs/bundle-$c.log 2>&1
  say "== bundle $c: exit=$? last=$(tail -1 bundle-logs/bundle-$c.log)"
  say "$(grep '^sha256' bundle-logs/bundle-$c.log)"
  ( cd /tmp/dist-$c && find . -printf '%M %s %p\n' | sort -k3 ) >> $out
  for d in /tmp/dist-$c/*.app/share/applications/*.desktop; do
    say "-- $(basename $d)"; cat "$d" >> $out
    say "desktop-file-validate: $(desktop-file-validate "$d" 2>&1 || echo FAILED) (exit ${PIPESTATUS[0]})"
  done
  for b in /tmp/dist-$c/*.app/bin/*; do
    say "debug info sections: $(readelf -S "$b" | grep -c debug_info)"
    say "newest glibc needed: $(objdump -T "$b" | grep -o 'GLIBC_[0-9.]*' | sort -uV | tail -1)"
    say "ldd not found: $(ldd "$b" | grep -c 'not found')"
  done
  for p in /tmp/dist-$c/*.app/share/icons/hicolor/*/apps/*.png; do say "icon $(identify -format '%wx%h' "$p") $p"; done
done

say "== install plain from the archive"
mkdir -p /tmp/unpack && tar -xzf /tmp/dist-plain/lab-*-linux-*.tar.gz -C /tmp/unpack
sh /tmp/unpack/lab.app/install.sh > install1.log 2>&1; say "install exit=$?"; cat install1.log >> $out
say "-- installed desktop file"; cat ~/.local/share/applications/test.yard.lab.desktop >> $out
say "desktop-file-validate installed: $(desktop-file-validate ~/.local/share/applications/test.yard.lab.desktop 2>&1) (exit $?)"
say "link: $(readlink ~/.local/bin/lab)"
say "xdg-mime default: $(xdg-mime query default x-scheme-handler/yardlab)"
sh /tmp/unpack/lab.app/install.sh > install2.log 2>&1; say "second install exit=$?"
say "leftovers in ~/.local: $(ls -a ~/.local | grep -v '^\.\.\?$' | tr '\n' ' ')"

export LAB_LIFECYCLE=info,yard_shell=debug
say "== xdg-open with no instance running: the app starts with the URL"
xdg-open 'yardlab://nobody/cold?token=secret' > xdg-cold.log 2>&1 &
sleep 10
say "running: $(pgrep -f '.local/lab.app/bin/lab' | wc -l)"
say "== xdg-open with the instance running: the URL is forwarded"
timeout 30 xdg-open 'yardlab://nobody/warm?token=secret' > xdg-warm.log 2>&1; say "xdg-open exit=$?"
sleep 3
say "running after forward: $(pgrep -f '.local/lab.app/bin/lab' | wc -l)"
WID=$(xdotool search --classname test.yard.lab | head -1); say "window $WID"
xprop -id "$WID" WM_CLASS _NET_WM_NAME >> $out 2>&1 || true
import -window root bundle-screen.png || true
pkill -f '.local/lab.app/bin/lab'; sleep 2
cp -r ~/.local/share/lab/logs bundle-logs/lab-logs 2>/dev/null || true
say "secret in any log: $(grep -rl secret xdg-cold.log xdg-warm.log bundle-logs 2>/dev/null | wc -l)"
say "-- cold start trace (instance and url lines)"
grep -E 'instance|url' xdg-cold.log >> $out || true
say "-- forwarding start trace"
cat xdg-warm.log >> $out

say "== end-to-end test"
cargo test -p xtask --test bundle_linux_end_to_end -- --ignored 2>&1 | tail -4 | tee -a $out
cargo test -p xtask 2>&1 | grep 'test result' | tee -a $out
