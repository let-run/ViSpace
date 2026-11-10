#!/usr/bin/env python3
"""Create a development CA and localhost/LAN server certificate. Never install trust automatically."""
import argparse, ipaddress, os, re, socket, subprocess
from pathlib import Path
parser=argparse.ArgumentParser()
parser.add_argument('--directory',type=Path,default=Path(__file__).resolve().parents[1]/'.local-tls')
parser.add_argument('--host',action='append',default=[],help='Additional LAN hostname or IP (repeatable)')
parser.add_argument('--node-id', help='Short node ID from Sync, for <id>.vispace.local')
parser.add_argument('--ca-directory', type=Path, help='Reuse an existing CA from this directory')
args=parser.parse_args();root=args.directory.resolve();root.mkdir(parents=True,exist_ok=True);os.chmod(root,0o700)
ca=args.ca_directory.resolve() if args.ca_directory else root
if args.ca_directory and not all((ca/name).is_file() for name in ['ca.pem','ca-key.pem']):
 parser.error('The CA directory must contain ca.pem and ca-key.pem.')
if args.node_id:
 node=args.node_id
 if not re.fullmatch(r'[a-f0-9]{24}',node):parser.error('Expected a 24-character node ID.')
 args.host.append(node+'.vispace.local')
if (not args.ca_directory and (root/'ca-key.pem').exists()) or (root/'server-key.pem').exists():
 parser.error('Certificate directory already contains keys. Choose a new directory to regenerate.')
hosts=['localhost','127.0.0.1','::1',socket.gethostname(),*args.host]
try: hosts.extend(socket.gethostbyname_ex(socket.gethostname())[2])
except OSError: pass
sans=[]
for h in dict.fromkeys(hosts):
 try: ipaddress.ip_address(h);sans.append('IP:'+h)
 except ValueError:
  if not h or any(c not in 'abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789.-' for c in h):parser.error('Invalid hostname')
  sans.append('DNS:'+h)
ext=root/'server.ext';ext.write_text('basicConstraints=critical,CA:FALSE\nkeyUsage=critical,digitalSignature\nextendedKeyUsage=serverAuth\nsubjectAltName='+','.join(sans)+'\n')
def run(*cmd):subprocess.run(['openssl',*cmd],check=True,cwd=root,stdout=subprocess.DEVNULL)
if not args.ca_directory:
 run('ecparam','-name','prime256v1','-genkey','-noout','-out','ca-key.pem')
 run('req','-x509','-new','-sha256','-key','ca-key.pem','-days','90','-out','ca.pem','-subj','/CN=VInventory Local Test CA','-addext','basicConstraints=critical,CA:TRUE','-addext','keyUsage=critical,keyCertSign,cRLSign')
run('ecparam','-name','prime256v1','-genkey','-noout','-out','server-key.pem')
run('req','-new','-key','server-key.pem','-out','server.csr','-subj','/CN=localhost')
run('x509','-req','-in','server.csr','-CA',str(ca/'ca.pem'),'-CAkey',str(ca/'ca-key.pem'),'-set_serial','0x'+os.urandom(16).hex(),'-days','30','-sha256','-extfile','server.ext','-out','server.pem')
os.chmod(root/'server-key.pem',0o600)
if not args.ca_directory:os.chmod(root/'ca-key.pem',0o600)
print(f'Trust this CA manually: {ca / "ca.pem"}\nServer certificate: {root / "server.pem"}\nPrivate key: {root / "server-key.pem"}\nNames: '+', '.join(hosts))
