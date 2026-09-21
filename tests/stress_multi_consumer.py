#!/usr/bin/env python3
from __future__ import annotations
import multiprocessing as mp
import os, random, time, json
import numpy as np
from tenzorbus import SharedTensorRing


def worker(name, count, ready, out, jitter):
    ring=SharedTensorRing.attach(name); c=ring.consumer(); ready.set()
    seqs=[]; bad=0
    try:
        for expected in range(1,count+1):
            with c.next(timeout=10) as lease:
                a=lease.numpy(); seqs.append(lease.meta.sequence)
                # Payload embeds sequence value in every element.
                if not np.all(a == np.float32(expected)):
                    bad += 1
                if jitter: time.sleep(random.random()*jitter)
                del a
        out.put({"pid":os.getpid(),"bad":bad,"first":seqs[0],"last":seqs[-1],"count":len(seqs),"strict":seqs==list(range(1,count+1))})
    finally:
        c.close(); ring.close(unlink=False)


def main(consumers=8,count=500,jitter=0.001):
    name=f"stress_{os.getpid()}_{time.time_ns()}"; shape=(3,64,64)
    ring=SharedTensorRing.create(name,slot_count=16,slot_capacity=np.zeros(shape,dtype=np.float32).nbytes+1024,force=True)
    out=mp.Queue(); events=[mp.Event() for _ in range(consumers)]; procs=[]
    for i in range(consumers):
        p=mp.Process(target=worker,args=(name,count,events[i],out,jitter if i%2 else 0)); p.start(); procs.append(p)
    assert all(e.wait(5) for e in events)
    started=time.perf_counter()
    try:
        for seq in range(1,count+1):
            ring.publish(np.full(shape,seq,dtype=np.float32),timeout=10)
        results=[out.get(timeout=20) for _ in procs]
    finally:
        for p in procs: p.join(20)
        stats=ring.stats(); ring.close(unlink=True)
    elapsed=time.perf_counter()-started
    ok=all(p.exitcode==0 for p in procs) and all(r['bad']==0 and r['strict'] for r in results)
    report={"ok":ok,"consumers":consumers,"publications":count,"consumer_deliveries":consumers*count,"elapsed_s":elapsed,"publications_per_s":count/elapsed,"results":results,"ring_stats":stats}
    print(json.dumps(report,indent=2))
    if not ok: raise SystemExit(1)

if __name__=='__main__': main()
