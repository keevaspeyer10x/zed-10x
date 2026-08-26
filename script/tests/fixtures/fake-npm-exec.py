#!/usr/bin/env python3
"""Stand in for npm exec while preserving the ACP child transport."""

import os
import sys


fixture = os.environ["FAKE_ACP_FIXTURE"]
os.execv("/usr/bin/python3", ["/usr/bin/python3", fixture, "--mode", "pass"])
