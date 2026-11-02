// Generic stream driver for a compiled block (isdb_fpga/chain/simx.py):
// CXXRTL's C API, or the same subset on a Verilated model (simv.py).
//
// The design is compiled together with this file into a shared library;
// Python (ctypes) names the ports, hands over the input records and the
// output groups, and `run` clocks the design until the inputs are used
// up and `tail` more cycles have passed. Per cycle: the current input
// record (if any) is put on the input ports with `valid` high; the
// combinational logic is settled with the clock low and `ready` read; the
// clock rises; every output group whose valid port is high after the edge
// appends its data ports' values (sign-extended as told) to its buffer.
// That is exactly the Amaranth testbench sequence ctx.set(...); await
// ctx.tick(); ctx.get(...) of isdb_fpga/chain/simx.py's pysim backend.

#include <cstdint>
#include <cstring>
#include <string>
#include <utility>
#include <vector>

#include <cxxrtl/capi/cxxrtl_capi.h>

extern "C" cxxrtl_toplevel cxxrtl_design_create();

struct Port {
	cxxrtl_object *obj;
	size_t width;
	int is_signed;
};

struct Group {
	int valid;
	std::vector<int> data;
	std::vector<int64_t> out;
};

// A faster clock related to `clk` (`<name>_clk`): `ratio` rising edges per
// period of clk, one of them coincident with clk's.
struct Fast {
	cxxrtl_object *clk;
	int ratio;
};

struct Harness {
	cxxrtl_handle h;
	cxxrtl_object *clk;
	std::vector<Fast> fast;
	// the edges inside one period of clk, after its edge: (time in
	// 1 / lcm units, clock index), in time order
	std::vector<std::pair<int, int>> sched;
	int period = 1;
	std::vector<Port> ports;
	std::vector<Group> groups;
	uint64_t cycles = 0;
};

static void put(cxxrtl_object *o, size_t width, int64_t v) {
	uint32_t *p = o->next ? o->next : o->curr;
	size_t chunks = (width + 31) / 32;
	for (size_t i = 0; i < chunks; i++) {
		uint32_t w = i < 2 ? (uint32_t)((uint64_t)v >> (32 * i)) : (v < 0 ? 0xFFFFFFFFu : 0);
		size_t bits = width - 32 * i;
		if (bits < 32)
			w &= (1u << bits) - 1;
		p[i] = w;
	}
}

static int64_t get(const Port &p) {
	uint64_t v = p.obj->curr[0];
	if (p.width > 32)
		v |= (uint64_t)p.obj->curr[1] << 32;
	if (p.width < 64) {
		v &= (1ull << p.width) - 1;
		if (p.is_signed && (v >> (p.width - 1)) & 1)
			v |= ~((1ull << p.width) - 1);
	}
	return (int64_t)v;
}

extern "C" {

void *h_create() {
	Harness *s = new Harness;
	s->h = cxxrtl_create(cxxrtl_design_create());
	s->clk = cxxrtl_get(s->h, "clk");
	cxxrtl_object *rst = cxxrtl_get(s->h, "rst");
	if (rst)
		put(rst, 1, 0);
	// settled before the first clock (no edge: the clock stays low), so a
	// read before it sees the combinational outputs of the initial state,
	// as on the Python simulator and on Verilator
	put(s->clk, 1, 0);
	cxxrtl_step(s->h);
	return s;
}

static int gcd(int a, int b) { return b ? gcd(b, a % b) : a; }

// Drive the domain `name` (its `<name>_clk` port, if the design has one) at
// `ratio` x clk, phase-aligned with clk (an edge of both at clk's edge).
// Returns 0 if the design has no such clock.
int h_fast_clock(void *p, const char *name, int ratio) {
	Harness *s = (Harness *)p;
	std::string base(name);
	cxxrtl_object *c = cxxrtl_get(s->h, (base + "_clk").c_str());
	if (!c)
		return 0;
	cxxrtl_object *r = cxxrtl_get(s->h, (base + "_rst").c_str());
	if (r)
		put(r, 1, 0);
	s->fast.push_back(Fast{c, ratio});
	int l = 1;
	for (auto &f : s->fast)
		l = l / gcd(l, f.ratio) * f.ratio;
	s->period = l;
	s->sched.clear();
	for (int t = 1; t < l; t++)
		for (size_t i = 0; i < s->fast.size(); i++)
			if (t % (l / s->fast[i].ratio) == 0)
				s->sched.push_back({t, (int)i});
	return 1;
}

void h_destroy(void *p) {
	Harness *s = (Harness *)p;
	cxxrtl_destroy(s->h);
	delete s;
}

// Index of port `name` (-1 if absent); `is_signed` sign-extends reads.
int h_port(void *p, const char *name, int is_signed) {
	Harness *s = (Harness *)p;
	cxxrtl_object *o = cxxrtl_get(s->h, name);
	if (!o)
		return -1;
	s->ports.push_back(Port{o, o->width, is_signed});
	return (int)s->ports.size() - 1;
}

void h_set(void *p, int port, int64_t v) {
	Harness *s = (Harness *)p;
	put(s->ports[port].obj, s->ports[port].width, v);
}

int64_t h_get(void *p, int port) {
	Harness *s = (Harness *)p;
	return get(s->ports[port]);
}

static void settle(Harness *s) {
	put(s->clk, 1, 0);
	for (auto &f : s->fast)
		put(f.clk, 1, 0);
	cxxrtl_step(s->h);
}

// The faster clocks' edges between clk's edges (the inputs of the cycle
// already applied, as an Amaranth testbench sets them right after clk's
// edge); none before the first edge of clk, as in Amaranth's simulator.
static void fast_edges(Harness *s) {
	if (s->sched.empty() || s->cycles == 0)
		return;
	size_t k = 0;
	while (k < s->sched.size()) {
		int t = s->sched[k].first;
		size_t j = k;
		for (; j < s->sched.size() && s->sched[j].first == t; j++)
			put(s->fast[s->sched[j].second].clk, 1, 1);
		cxxrtl_step(s->h);
		for (size_t i = k; i < j; i++)
			put(s->fast[s->sched[i].second].clk, 1, 0);
		cxxrtl_step(s->h);
		k = j;
	}
}

static void edge(Harness *s) {
	fast_edges(s);
	put(s->clk, 1, 1);
	for (auto &f : s->fast)
		put(f.clk, 1, 1);
	cxxrtl_step(s->h);
#ifndef HARNESS_EDGE_SETTLES
	// CXXRTL's step evaluates the logic once and commits the registers
	// after it: an output combinational from registers would still show
	// its value from before the edge. A second step (the clock still high,
	// no edge) settles it. A Verilated model's eval() settles by itself
	// (simv.py's adapter defines HARNESS_EDGE_SETTLES).
	cxxrtl_step(s->h);
#endif
	s->cycles++;
}

// One clock: settle, rise, collect the groups.
static void tick(Harness *s) {
	settle(s);
	edge(s);
	for (auto &g : s->groups) {
		if (get(s->ports[g.valid])) {
			for (int d : g.data)
				g.out.push_back(get(s->ports[d]));
		}
	}
}

void h_tick(void *p, int n) {
	Harness *s = (Harness *)p;
	for (int i = 0; i < n; i++)
		tick(s);
}

int h_group(void *p, int valid, const int *data, int n) {
	Harness *s = (Harness *)p;
	Group g;
	g.valid = valid;
	g.data.assign(data, data + n);
	s->groups.push_back(g);
	return (int)s->groups.size() - 1;
}

int64_t h_group_len(void *p, int g) {
	return (int64_t)((Harness *)p)->groups[g].out.size();
}

void h_group_read(void *p, int g, int64_t *dst) {
	Harness *s = (Harness *)p;
	std::memcpy(dst, s->groups[g].out.data(), s->groups[g].out.size() * sizeof(int64_t));
	s->groups[g].out.clear();
}

uint64_t h_cycles(void *p) {
	return ((Harness *)p)->cycles;
}

// Feed `n` records of `k` fields (row-major) through ports `fields`, with
// `valid` high while a record is presented and `ready` (or -1: always)
// accepting it at the edge; `gap[i]` idle cycles after record i (NULL:
// none). Then `tail` cycles. Returns the cycles used. A record that waits
// more than h_stall_limit cycles stops the run (h_stalled says at which).
static int64_t stall_limit = 50000000;
static int64_t stalled = -1;

void h_stall_limit(int64_t n) { stall_limit = n; }
int64_t h_stalled() { return stalled; }

uint64_t h_run(void *p, int valid, int ready, const int *fields, int k, const int64_t *rec,
               int64_t n, const int32_t *gap, int64_t tail) {
	Harness *s = (Harness *)p;
	uint64_t start = s->cycles;
	stalled = -1;
	for (int64_t i = 0; i < n; i++) {
		int64_t waited = 0;
		for (int f = 0; f < k; f++)
			put(s->ports[fields[f]].obj, s->ports[fields[f]].width, rec[i * k + f]);
		put(s->ports[valid].obj, 1, 1);
		for (;;) {
			settle(s);
			bool ok = ready < 0 || get(s->ports[ready]);
			edge(s);
			for (auto &g : s->groups) {
				if (get(s->ports[g.valid])) {
					for (int d : g.data)
						g.out.push_back(get(s->ports[d]));
				}
			}
			if (ok)
				break;
			if (++waited > stall_limit) {
				stalled = i;
				return s->cycles - start;
			}
		}
		put(s->ports[valid].obj, 1, 0);
		if (gap)
			for (int32_t j = 0; j < gap[i]; j++)
				tick(s);
	}
	for (int64_t j = 0; j < tail; j++)
		tick(s);
	return s->cycles - start;
}

}
