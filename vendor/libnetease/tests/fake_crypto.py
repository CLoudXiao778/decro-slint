import subprocess, os
class _Cipher:
    MODE_CBC = 1; MODE_ECB = 2
    def __init__(self, key, mode, iv=None):
        self.key = key; self.mode = mode; self.iv = iv
    def _run(self, data, decrypt):
        algo = '-aes-128-cbc' if self.mode == self.MODE_CBC else '-aes-128-ecb'
        args = ['openssl', 'enc', algo, '-K', self.key.hex(), '-nopad']
        if decrypt: args.append('-d')
        if self.iv: args += ['-iv', self.iv.hex()]
        r = subprocess.run(args, input=data, capture_output=True)
        if r.returncode != 0: raise RuntimeError(r.stderr)
        return r.stdout
    def encrypt(self, data):
        return self._run(data, False)
    def decrypt(self, data):
        return self._run(data, True)
import sys, types
_c = types.ModuleType('Crypto'); _cc = types.ModuleType('Crypto.Cipher')
class _AES:
    MODE_CBC = 1; MODE_ECB = 2
    @staticmethod
    def new(key, mode, iv=None):
        return _Cipher(key, mode, iv)
_cc.AES = _AES
_c.Cipher = _cc
sys.modules['Crypto'] = _c; sys.modules['Crypto.Cipher'] = _cc
