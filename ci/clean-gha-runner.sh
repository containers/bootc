#!/bin/bash
set -xeuo pipefail
df -h
docker image prune --all --force > /dev/null
rm -rf /usr/share/dotnet /opt/ghc /usr/local/lib/android
apt-get remove -y '^aspnetcore-.*' > /dev/null
apt-get remove -y '^dotnet-.*' > /dev/null
apt-get remove -y '^llvm-.*' > /dev/null
apt-get remove -y 'php.*' > /dev/null
apt-get remove -y '^mongodb-.*' > /dev/null
apt-get remove -y '^mysql-.*' > /dev/null1
apt-get remove -y azure-cli google-chrome-stable firefox mono-devel >/dev/null
df -h
