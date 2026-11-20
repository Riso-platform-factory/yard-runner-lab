#!/usr/bin/env bash
# Lab: one instance and URLs from the command line, end to end.
set -u
out=instance.txt; : > $out
say() { echo "$*" | tee -a $out; }
export YARD_HOME=$(mktemp -d) YARD_LIFECYCLE=info,yard_shell=debug
Y=target/debug/yard
$Y > first.log 2>&1 & FIRST=$!; sleep 6
$Y "yard://nobody/x?token=secret" > second.log 2>&1; say "second start exit=$?"
sleep 1; $Y > third.log 2>&1; say "third start exit=$?"
sleep 1
say "first still running: $(kill -0 $FIRST 2>/dev/null && echo yes || echo no)"
say "socket: $(ls $YARD_HOME/data/instance.sock 2>&1)"
say "secret in any log: $(grep -l secret first.log second.log third.log 2>/dev/null | wc -l)"
kill -9 $FIRST; sleep 1
say "after kill -9, socket left: $(ls $YARD_HOME/data/instance.sock 2>/dev/null && echo yes || echo no)"
$Y "yard://nobody/again" > fourth.log 2>&1 & FOURTH=$!; sleep 6
say "fourth start (after a stale socket) running: $(kill -0 $FOURTH 2>/dev/null && echo yes || echo no)"
kill $FOURTH; sleep 2
say "after a normal quit, socket left: $(ls $YARD_HOME/data/instance.sock 2>/dev/null && echo yes || echo no)"
