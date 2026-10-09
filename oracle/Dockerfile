# Oracle container for the gr-isdbt differential tests (oracle/run_matrix.py).
#
# Ubuntu 24.04 with its GNU Radio 3.10 packages (3.10.9.2 when this was
# written) and gr-osmosdr, plus gr-isdbt built from its upstream repository
# at a pinned commit. GNU Radio and gr-isdbt are GPL-3.0 programs; this file
# only fetches and builds them, nothing of them is part of isdb-rs.
#
#   docker build -t isdb-oracle oracle/
#   docker run -d --name isdb-oracle --user "$(id -u):$(id -g)" \
#       -v "$PWD":/work isdb-oracle
#   python3 oracle/run_matrix.py
#
# See docs/test-data.md ("The gr-isdbt oracle").
FROM ubuntu:24.04

# gr-isdbt upstream and the commit the published results were produced with.
ARG GR_ISDBT_REPO=https://github.com/git-artes/gr-isdbt.git
ARG GR_ISDBT_COMMIT=56b2556c14ecc5d710070f969fda7a2deae65d8b

ENV DEBIAN_FRONTEND=noninteractive
RUN apt-get update \
 && apt-get install -y --no-install-recommends \
      ca-certificates git cmake make g++ pkg-config \
      gnuradio gnuradio-dev gr-osmosdr \
      pybind11-dev python3-numpy python3-pygccxml \
      libspdlog-dev libfmt-dev libgmp-dev liborc-0.4-dev libsndfile1-dev \
 && rm -rf /var/lib/apt/lists/*

RUN git init -q /tmp/gr-isdbt \
 && cd /tmp/gr-isdbt \
 && git remote add origin "$GR_ISDBT_REPO" \
 && git fetch -q --depth 1 origin "$GR_ISDBT_COMMIT" \
 && git checkout -q FETCH_HEAD \
 && test "$(git rev-parse HEAD)" = "$GR_ISDBT_COMMIT" \
 && cmake -S . -B build -DCMAKE_BUILD_TYPE=Release -DENABLE_DOXYGEN=OFF \
 && cmake --build build -j"$(nproc)" \
 && cmake --install build \
 && ldconfig \
 && cd / && rm -rf /tmp/gr-isdbt \
 && python3 -c "import gnuradio.isdbt, gnuradio.dtv"

# The repository is mounted here; run_matrix.py execs the scripts in it.
# HOME is writable for any uid passed with --user (GNU Radio writes its
# preferences there).
ENV HOME=/tmp
WORKDIR /work
CMD ["sleep", "infinity"]
