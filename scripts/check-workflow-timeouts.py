#!/usr/bin/env python3
"""Every job in .github/workflows must set timeout-minutes, so a hung job
cannot run until the six-hour default limit."""

import re
import sys
from pathlib import Path

bad = []
files = sorted(Path(".github/workflows").glob("*.yml")) + sorted(Path(".github/workflows").glob("*.yaml"))
for f in files:
    job = None
    has_timeout = False
    in_jobs = False
    pending = []
    for line in f.read_text().splitlines():
        if re.match(r"^jobs:\s*$", line):
            in_jobs = True
            continue
        if in_jobs and re.match(r"^[a-zA-Z_]", line):
            in_jobs = False
        if not in_jobs:
            continue
        m = re.match(r"^  ([^#\s][^:]*):\s*$", line)
        if m:
            if job is not None and not has_timeout:
                pending.append(job)
            job = m.group(1).strip("\"'")
            has_timeout = False
            continue
        if job is not None and re.match(r"^    timeout-minutes:", line):
            has_timeout = True
    if job is not None and not has_timeout:
        pending.append(job)
    bad.extend(f"{f}: {j}" for j in pending)

if bad:
    print("jobs without timeout-minutes:")
    print("\n".join(f"  {b}" for b in bad))
    sys.exit(1)
print("every workflow job sets timeout-minutes")
