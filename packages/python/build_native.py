"""Copy an already-built local extension into the development package.

Run: cargo build -p telemetry-python --features extension-module
     python3 packages/python/build_native.py [--release]
"""
import argparse
import pathlib
import shutil
import sys
import sysconfig

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument("--release", action="store_true")
args = parser.parse_args()
root = pathlib.Path(__file__).resolve().parents[2]
profile = "release" if args.release else "debug"
source = root / "target" / profile / ("_native.dll" if sys.platform == "win32" else "lib_native.dylib" if sys.platform == "darwin" else "lib_native.so")
target = pathlib.Path(__file__).resolve().parent / "telemetry" / ("_native" + sysconfig.get_config_var("EXT_SUFFIX"))
shutil.copyfile(source, target)
print(target)
