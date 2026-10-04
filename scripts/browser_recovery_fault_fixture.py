"""Test-only worker wrapper, mounted by the isolated acceptance runner."""
import os
from pathlib import Path
import sys
import time

sys.path.insert(0, "/opt/blog/scripts")
import browser_recovery

original = browser_recovery.Store.phase


def phase(self, value, **extra):
    original(self, value, **extra)
    marker = self.root / "TEST_INTERRUPT_RESTORE"
    if value == "files" and marker.exists():
        marker.unlink()
        # The test restarts the container while the journal says files. No test
        # hook is shipped in the application image or enabled by a web request.
        time.sleep(120)


browser_recovery.Store.phase = phase
sys.exit(browser_recovery.main())
