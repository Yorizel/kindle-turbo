"""Exercise the USB installer against owned temporary files, without a device."""

import os
from pathlib import Path
import shutil
import subprocess
import tempfile

REPO = Path(__file__).resolve().parents[2]
INSTALLER = REPO / "scripts/install-koplugin.sh"


def run(root, env=None):
    return subprocess.run(["sh", str(INSTALLER), str(root)], env=env, capture_output=True, text=True, check=False)


def device(root):
    (root / "documents").mkdir(parents=True)
    (root / "koreader/plugins").mkdir(parents=True)
    (root / "koreader/settings").mkdir()
    (root / "koreader/settings/keep.lua").write_text("preserve existing settings")
    return root


with tempfile.TemporaryDirectory(prefix="kindle-turbo-installer-") as fixture:
    fixture = Path(fixture)
    root = device(fixture / "Kindle com espaços")
    target = root / "koreader/plugins/kindle-turbo.koplugin"
    assert run(root).returncode == 0
    for source in (REPO / "koplugin/kindle-turbo.koplugin").iterdir():
        assert (target / source.name).read_bytes() == source.read_bytes()
    assert (target / "LICENSE-TachiKindle-Apache-2.0.txt").read_bytes() == (REPO / "third-party/TachiKindle-LICENSE").read_bytes()
    assert run(root).returncode != 0
    assert run(fixture).returncode != 0
    assert (root / "koreader/settings/keep.lua").read_text() == "preserve existing settings"
    assert not list((root / "koreader/plugins").glob(".*partial*"))

    # A failed copy must clean staging without touching a pre-existing partial.
    failed = device(fixture / "failed")
    foreign = failed / "koreader/plugins/.kindle-turbo.koplugin.partial.foreign"
    foreign.mkdir()
    (foreign / "keep").write_text("another installer")
    fakebin = fixture / "bin"
    fakebin.mkdir()
    fakecp = fakebin / "cp"
    fakecp.write_text("#!/bin/sh\nexit 1\n")
    fakecp.chmod(0o755)
    env = dict(os.environ, PATH=str(fakebin) + os.pathsep + os.environ["PATH"])
    assert run(failed, env).returncode != 0
    assert list((failed / "koreader/plugins").glob(".*partial*")) == [foreign]
    assert (foreign / "keep").read_text() == "another installer"
    fakecp.unlink()

    # A competing install appearing after the first check cannot be overwritten/nested.
    raced = device(fixture / "raced")
    raced_target = raced / "koreader/plugins/kindle-turbo.koplugin"
    real_mv = shutil.which("mv")
    assert real_mv
    fakemv = fakebin / "mv"
    fakemv.write_text('#!/bin/sh\nmkdir -- "$RACED_TARGET"\nprintf preserve > "$RACED_TARGET/keep"\nexec "$REAL_MV" "$@"\n')
    fakemv.chmod(0o755)
    env.update(RACED_TARGET=str(raced_target), REAL_MV=real_mv)
    assert run(raced, env).returncode != 0
    assert (raced_target / "keep").read_text() == "preserve"
    assert list(raced_target.iterdir()) == [raced_target / "keep"]
    assert not list((raced / "koreader/plugins").glob(".*partial*"))

print("PASS USB installer: copy, licenses, existing installs, failures, staging ownership and races")
