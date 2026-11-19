import importlib.util
from pathlib import Path
import socket
import struct
import unittest
from unittest.mock import patch
spec=importlib.util.spec_from_file_location('load',Path(__file__).with_name('node-portable-load.py'))
load=importlib.util.module_from_spec(spec); spec.loader.exec_module(load)

class Fixtures(unittest.TestCase):
    def test_cpu_formats(self):
        self.assertEqual(load.cpu_seconds('01:02.25'),62.25)
        self.assertEqual(load.cpu_seconds('01:02:03'),3723)
        self.assertEqual(load.cpu_seconds('2-01:02:03'),176523)
    def test_ps_filters_unowned_reused_child(self):
        node=type('Node',(),{'pid':1})()
        with patch.object(load.subprocess,'check_output',return_value=b'1 0 100 00:01\n2 1 20 00:02\n3 9 900 00:03\n'):
            sample=load.sample(node,[2,3])
        self.assertEqual(sample['node_rss_kib'],100)
        self.assertEqual(sample['children_rss_kib'],20)
    def test_oversized_frame_rejected_before_payload(self):
        a,b=socket.socketpair(); self.addCleanup(a.close); self.addCleanup(b.close)
        a.settimeout(.1); ws=object.__new__(load.WS); ws.sock=a
        b.sendall(b'\x81\x7f'+struct.pack('!Q',2**40))
        with self.assertRaises(ValueError): ws.recv()
    def test_eof_and_fragment_rejected(self):
        for header in [b'',b'\x01\x00']:
            a,b=socket.socketpair(); a.settimeout(.1)
            try:
                b.sendall(header); b.close()
                ws=object.__new__(load.WS); ws.sock=a
                with self.assertRaises((EOFError,ValueError)): ws.recv()
            finally: a.close(); b.close()

class ReviewRegressions(unittest.TestCase):
    def test_control_redirect_is_rejected_without_a_followup(self):
        with self.assertRaisesRegex(ValueError, 'redirects are forbidden'):
            load.NoRedirect().redirect_request(None, None, 302, 'Found', {},
                                               'https://unrelated.invalid/')

    def test_retention_rejects_prefix_loss_and_empty_small_history(self):
        load.validate_retention(None,128,128,False)
        load.validate_retention({'from':1,'to':120},8,128,True)
        for gap, kept, small in [({'from':1,'to':128},0,False), ({'from':1,'to':128},0,True), (None,127,False), ({'from':1,'to':119},8,True)]:
            with self.assertRaises(ValueError): load.validate_retention(gap,kept,128,small)

    def test_whole_exact_deadline_cannot_be_extended_by_byte_trickle(self):
        clock=[0.0]
        class Drip:
            def settimeout(self, timeout): pass
            def recv(self, size): clock[0]+=.4; return b'x'
        with patch.object(load.time,'monotonic',side_effect=lambda:clock[0]):
            with self.assertRaises(TimeoutError): load.exact(Drip(),10,deadline=1)
        self.assertLess(clock[0],2)

    def test_http_body_trickle_has_absolute_deadline(self):
        clock=[0.0]
        class Drip:
            def read1(self, size): clock[0]+=1; return b'x'
        with patch.object(load.time,'monotonic',side_effect=lambda:clock[0]):
            with self.assertRaises(TimeoutError): load.http_body(Drip())
        self.assertEqual(clock[0],3)

    def test_abort_interrupts_registered_recv_and_rejects_late_registration(self):
        import threading
        guard=load.SocketGuard(); a,b=socket.socketpair()
        self.addCleanup(a.close);self.addCleanup(b.close)
        guard.register(a); done=threading.Event()
        def blocked():
            try: load.exact(a,1)
            except (EOFError,OSError): done.set()
        thread=threading.Thread(target=blocked);thread.start()
        guard.abort();self.assertTrue(done.wait(1));thread.join(timeout=1)
        c,d=socket.socketpair();self.addCleanup(c.close);self.addCleanup(d.close)
        with self.assertRaises(RuntimeError):guard.register(c)
        d.settimeout(.1);self.assertEqual(d.recv(1),b'')

if __name__=='__main__': unittest.main()
