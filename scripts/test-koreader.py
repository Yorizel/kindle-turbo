"""Run plugin integration tests with a real KOReader application directory."""

import os
from pathlib import Path
import subprocess
import sys
import tempfile

repo = Path(__file__).resolve().parents[1]
if len(sys.argv) != 2:
    raise SystemExit("Uso: python3 scripts/test-koreader.py /caminho/do/koreader (contendo reader.lua e luajit)")
runtime = Path(sys.argv[1]).resolve()
if not (runtime / "reader.lua").is_file() or not (runtime / "luajit").is_file():
    raise SystemExit("Diretório KOReader inválido")
subprocess.run([sys.executable, str(repo / "tests/koreader/test_installer.py")], check=True)
with tempfile.TemporaryDirectory(prefix="kindle-turbo-koreader-") as fixture:
    env = dict(os.environ, KO_HOME=fixture + "/ko-home")
    subprocess.run(
        [str(runtime / "luajit"), str(repo / "tests/koreader/test_plugin.lua"), str(repo), fixture],
        cwd=runtime,
        env=env,
        check=True,
    )
