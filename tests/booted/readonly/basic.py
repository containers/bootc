# Tests which are read-only/nondestructive

import json
import subprocess

def run(*args):
    subprocess.check_call(*args)

def test_bootc_status():
    o = subprocess.check_output(["bootc", "status", "--json"])
    st = json.loads(o)
    assert st['apiVersion'] == 'org.containers.bootc/v1'
    for v in [0, 1]:
        o = subprocess.check_output(["bootc", "status", "--json", f"--format-version={v}"])
        st = json.loads(o)
        assert st['apiVersion'] == 'org.containers.bootc/v1'

def test_bootc_status_invalid_version():
    o = subprocess.call(["bootc", "status", "--json", "--format-version=42"])
    assert o != 0
