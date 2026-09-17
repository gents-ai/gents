#!/bin/sh
if [ -w /host/api-work ]; then
  printf 'Content-Type: text/plain\r\n\r\nhealthy\n'
else
  printf 'Status: 503 Service Unavailable\r\nContent-Type: text/plain\r\n\r\nworking directory is not writable\n'
fi
