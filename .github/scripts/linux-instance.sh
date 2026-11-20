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
say "socket: $(stat -c '%A %U %n' $YARD_HOME/data/instance/instance.sock 2>&1)"
say "folder: $(stat -c '%A %U %n' $YARD_HOME/data/instance 2>&1)"
say "secret in any log: $(grep -l secret first.log second.log third.log 2>/dev/null | wc -l)"
kill -9 $FIRST; sleep 1
say "after kill -9, socket left: $(ls $YARD_HOME/data/instance/instance.sock 2>/dev/null && echo yes || echo no)"
$Y "yard://nobody/again" > fourth.log 2>&1 & FOURTH=$!; sleep 6
say "fourth start (after a stale socket) running: $(kill -0 $FOURTH 2>/dev/null && echo yes || echo no)"
kill $FOURTH; sleep 2
say "after a normal quit, socket left: $(ls $YARD_HOME/data/instance/instance.sock 2>/dev/null && echo yes || echo no)"
# A regular file where the socket would be is never deleted.
export YARD_HOME=$(mktemp -d); mkdir -p $YARD_HOME/data/instance; echo keep > $YARD_HOME/data/instance/instance.sock
$Y > fifth.log 2>&1 & FIFTH=$!; sleep 6
say "regular file kept: $(cat $YARD_HOME/data/instance/instance.sock 2>/dev/null)"; kill $FIFTH; sleep 1
# Two starts at the same moment: exactly one keeps running.
export YARD_HOME=$(mktemp -d)
$Y > race1.log 2>&1 & A=$!; $Y > race2.log 2>&1 & B=$!; sleep 8
say "race: running=$( (kill -0 $A 2>/dev/null && echo A); (kill -0 $B 2>/dev/null && echo B) | tr '\n' ' ')"
kill $A $B 2>/dev/null; sleep 1
# Under umask 002, a fresh install keeps one instance.
export YARD_HOME=$(mktemp -d)
( umask 002; $Y > umask1.log 2>&1 & echo $! > /tmp/u1 ); sleep 6
( umask 002; $Y "yard://nobody/u" > umask2.log 2>&1; echo $? > /tmp/u2 )
say "umask 002: data folder $(stat -c '%A' $YARD_HOME/data), second start exit=$(cat /tmp/u2)"
kill $(cat /tmp/u1); sleep 1
# A group-writable data folder of another group: run alone, with a warning.
export YARD_HOME=$(mktemp -d); mkdir -p $YARD_HOME/data; chmod 0775 $YARD_HOME/data; sudo chgrp adm $YARD_HOME/data
$Y > othergroup.log 2>&1 & OG=$!; sleep 6
say "other group: $(stat -c '%A %G' $YARD_HOME/data), running=$(kill -0 $OG 2>/dev/null && echo yes || echo no), warn=$(grep -c 'WARN.*instance' othergroup.log)"
kill $OG; sleep 1
