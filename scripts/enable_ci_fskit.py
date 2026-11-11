"""Enable macFUSE for FSKit tests on disposable GitHub-hosted macOS runners."""

from __future__ import annotations

import os
from pathlib import Path
import plistlib
import subprocess
import sys
import tempfile

MODULES = (
    "io.macfuse.app.fsmodule.macfuse",
    "io.macfuse.app.fsmodule.macfuse-local",
)


def write_atomic(path: Path, contents: bytes) -> None:
    """Replace the preference only after the complete new plist is on disk."""
    path.parent.mkdir(parents=True, exist_ok=True)
    temporary = None
    try:
        with tempfile.NamedTemporaryFile(dir=path.parent, delete=False) as file:
            temporary = Path(file.name)
            file.write(contents)
        temporary.replace(path)
    finally:
        if temporary is not None:
            temporary.unlink(missing_ok=True)


def enable(path: Path) -> tuple[bool, bytes | None]:
    """Add our two modules while preserving other enabled filesystem modules."""
    original = path.read_bytes() if path.exists() else None
    modules = plistlib.loads(original) if original is not None else []
    if not isinstance(modules, list) or not all(isinstance(item, str) for item in modules):
        raise ValueError("FSKit's enabled-module preference is not an array of strings")
    added = [module for module in MODULES if module not in modules]
    if not added:
        return False, original
    write_atomic(path, plistlib.dumps(modules + added))
    return True, original


def main() -> None:
    """Configure this job's runner; normal installations use System Settings."""
    if not (
        sys.platform == "darwin"
        and os.environ.get("GITHUB_ACTIONS") == "true"
        and os.environ.get("RUNNER_ENVIRONMENT") == "github-hosted"
    ):
        raise RuntimeError("This setup is only for disposable GitHub-hosted macOS runners")
    for module in MODULES:
        subprocess.run(["/usr/bin/pluginkit", "-e", "use", "-i", module], check=True)
    path = Path.home() / "Library/Group Containers/group.com.apple.fskit.settings/enabledModules.plist"
    changed, original = enable(path)
    if changed:
        try:
            # fskitd reads this preference on startup. Exit 1 means it was not
            # running; launchd will start it when the first mount is requested.
            restarted = subprocess.run(["sudo", "/usr/bin/pkill", "-x", "fskitd"], check=False)
            if restarted.returncode not in (0, 1):
                restarted.check_returncode()
        except BaseException:
            if original is None:
                path.unlink(missing_ok=True)
            else:
                write_atomic(path, original)
            raise
    print("FSKit enabled modules: " + ", ".join(MODULES))


if __name__ == "__main__":
    main()
