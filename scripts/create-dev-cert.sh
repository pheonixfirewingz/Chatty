#!/bin/sh
set -eu
mkdir -p certs
openssl req -x509 -newkey rsa:3072 -sha256 -nodes -days 3650 \
  -keyout certs/ca.key -out certs/ca.pem -subj '/CN=Chatty Local CA' \
  -addext 'basicConstraints=critical,CA:TRUE' \
  -addext 'keyUsage=critical,keyCertSign,cRLSign'
openssl req -new -newkey rsa:3072 -nodes -keyout certs/server.key \
  -out certs/server.csr -subj '/CN=localhost'
openssl x509 -req -in certs/server.csr -CA certs/ca.pem -CAkey certs/ca.key \
  -CAcreateserial -out certs/server.pem -days 365 -sha256 \
  -extfile scripts/dev-cert.ext
chmod 600 certs/ca.key certs/server.key
printf '%s\n' 'Created pinned certs/ca.pem plus certs/server.pem and certs/server.key'
