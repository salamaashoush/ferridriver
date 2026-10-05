#!/usr/bin/env python3
import os
import pathlib
import socket
import sys
import threading

args = sys.argv[1:]
if args[0] == 'keygen':
    pathlib.Path(args[1]).write_text('fixture key')
elif 'server' in args:
    home = pathlib.Path(os.environ['ANDROID_USER_HOME'])
    if any(os.environ.get(name) != '0' for name in ['ADB_USB', 'ADB_EMU', 'ADB_MDNS_AUTO_CONNECT']):
        raise RuntimeError('private server could discover unowned devices')
    if pathlib.Path(os.environ['HOME']) != home:
        raise RuntimeError('private server could read shared authentication state')
    (home / 'pid').write_text(str(os.getpid()))
    if (home / 'stall').exists():
        threading.Event().wait()
    listener = args[args.index('-L') + 1]
    if not listener.startswith('acceptfd:'):
        raise RuntimeError('server did not inherit its reserved listener')
    with socket.socket(fileno=int(listener.split(':')[1])) as server:
        port = server.getsockname()[1]
        with socket.socket() as contender:
            try:
                contender.bind(('127.0.0.1', port))
            except OSError:
                pass
            else:
                raise RuntimeError('listening port was not reserved')
        os.write(int(args[args.index('--reply-fd') + 1]), b'OK\n')
        while True:
            connection, _ = server.accept()
            with connection:
                if connection.recv(1024):
                    connection.sendall(b'device\n')
else:
    port = int(args[args.index('-P') + 1])
    try:
        with socket.create_connection(('127.0.0.1', port), timeout=1) as connection:
            connection.sendall(b'get-state')
            print(connection.recv(1024).decode().strip())
    except OSError:
        if '-H' in args and args[args.index('-H') + 1] == '127.0.0.1':
            raise RuntimeError('owned ADB server is unavailable') from None
        pathlib.Path(os.environ['ADB_VENDOR_KEYS']).with_name('unowned-replacement').write_text('spawned')
        print('replacement')
