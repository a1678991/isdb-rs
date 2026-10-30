#!/usr/bin/env python3
"""Continuity check across a looped TS file's seam (docs/plans/isdbs-next/pcr-pacing.md):
for each PID, does the first packet's continuity counter follow the last one's when
the file is played again from the start?

Usage: ts-loop-seam.py IN.ts
"""
import sys
d=open(sys.argv[1],'rb').read(); n=len(d)//188
def pk(i): return d[i*188:(i+1)*188]
def pid(p): return ((p[1]&0x1f)<<8)|p[2]
def pcr(p):
    if not(p[3]&0x20) or p[4]==0 or not(p[5]&0x10): return None
    b=p[6:12]; base=(b[0]<<25)|(b[1]<<17)|(b[2]<<9)|(b[3]<<1)|(b[4]>>7); return base*300+(((b[4]&1)<<8)|b[5])
first={};last={};pcrs={}
for i in range(n):
    p=pk(i); q=pid(p)
    if q==0x1fff: continue
    cc=p[3]&15; pay=bool(p[3]&0x10)
    first.setdefault(q,(i,cc,pay)); last[q]=(i,cc,pay)
    v=pcr(p)
    if v is not None: pcrs.setdefault(q,[]).append((i,v))
print("packets",n)
for q in sorted(first):
    fi,fc,fp=first[q]; li,lc,lp=last[q]
    exp=(lc+1)%16 if fp else lc
    print(f"pid {q:#06x} first@{fi} cc{fc} last@{li} cc{lc} -> next pass cc {fc} expected {exp} {'OK' if exp==fc else 'CC ERROR'}")
for q,s in pcrs.items():
    (i0,v0),(i1,v1)=s[0],s[-1]; tpp=(v1-v0)/(i1-i0)
    print(f"PCR pid {q:#06x}: first@{i0} {v0} last@{i1} {v1}; ticks/pkt {tpp:.4f}; period by packets {n*tpp:.1f} ticks = {n*tpp/27e6:.6f} s; seam gap {(v0+n*tpp-v1)/27e6*1e3:.3f} ms vs interval {(s[1][1]-v0)/27e6*1e3:.3f} ms")
