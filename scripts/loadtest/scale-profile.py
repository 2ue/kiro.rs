#!/usr/bin/env python3
"""Scale an overload-probe profile from the production pool (228 slots) to another pool size.

Usage: scale-profile.py PROFILE SLOTS
"""
import sys

PRODUCTION_SLOTS = 57 * 4

profile, slots = sys.argv[1], int(sys.argv[2])
factor = slots / PRODUCTION_SLOTS
segments = []
for segment in profile.split(","):
    kind, *numbers = segment.split(":")
    # Rate fields: steady/spike rate; ramp from,to; square low,high. Durations stay as they are.
    rate_fields = {"steady": 1, "spike": 1, "ramp": 2, "square": 2}.get(kind, 0)
    for index in range(rate_fields):
        numbers[index] = f"{float(numbers[index]) * factor:g}"
    segments.append(":".join([kind, *numbers]))
print(",".join(segments))
