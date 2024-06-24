# Tests which are read-only/nondestructive

import json
import subprocess

def run(*args):
    subprocess.check_call(*args)

def test_bootc_status():
    o = subprocess.check_output(["bootc", "status", "--json"])
    st = json.loads(o)
    assert st['apiVersion'] == 'org.containers.bootc/v1alpha1'
