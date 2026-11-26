#!/usr/bin/env python3
"""Print an app's AT-SPI tree (role: name, states), as a screen reader sees it.

usage: a11y-tree.py APP_NAME   (run on the same session bus as the app)
"""
import sys

import gi

gi.require_version("Atspi", "2.0")
from gi.repository import Atspi  # noqa: E402

STATES = ("checked", "selected", "expanded", "focusable", "pressed", "sensitive")


def walk(o, depth=0):
    st = o.get_state_set()
    flags = [s for s in STATES if s != "sensitive" and st.contains(getattr(Atspi.StateType, s.upper()))]
    if not st.contains(Atspi.StateType.SENSITIVE) and o.get_role_name() != "application":
        flags.append("disabled")
    print(f"{'  ' * depth}{o.get_role_name()}: {o.get_name()!r} {' '.join(flags)}".rstrip())
    for i in range(o.get_child_count()):
        walk(o.get_child_at_index(i), depth + 1)


desk = Atspi.get_desktop(0)
apps = [desk.get_child_at_index(i) for i in range(desk.get_child_count())]
found = [a for a in apps if a.get_name() == sys.argv[1]]
if not found:
    sys.exit(f"no app {sys.argv[1]!r} on the a11y bus (have {[a.get_name() for a in apps]})")
for a in found:
    walk(a)
