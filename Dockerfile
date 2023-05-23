FROM docker.io/library/alpine:edge AS builder
ENV RUST_TARGET "x86_64-unknown-linux-musl"
ENV RUSTFLAGS "-Lnative=/usr/lib -Z mir-opt-level=3"

RUN apk upgrade && \
    apk add curl gcc g++ musl-dev cmake make && \
    curl -sSf https://sh.rustup.rs | sh -s -- --profile minimal --component rust-src --default-toolchain nightly -y

WORKDIR /build

COPY Cargo.toml Cargo.lock ./
COPY .cargo ./.cargo/

WORKDIR /build/drill-server
COPY drill-server/Cargo.toml ./

WORKDIR /build/drill-proto
COPY drill-proto/Cargo.toml ./

WORKDIR /build/drill-client
COPY drill-client/Cargo.toml ./

WORKDIR /build

RUN mkdir drill-proto/src/ && \
    mkdir drill-server/src/ && \
    mkdir drill-client/src/ && \
    echo '' > ./drill-proto/src/lib.rs && \
    echo 'fn main() {}' > ./drill-server/src/main.rs && \
    echo 'fn main() {}' > ./drill-client/src/main.rs && \
    source $HOME/.cargo/env && \
    cargo build --release --features dynamic --target="$RUST_TARGET"

RUN rm -f target/$RUST_TARGET/release/deps/drill* && \
    rm -f target/$RUST_TARGET/release/deps/libdrill*

COPY ./drill-proto/src ./drill-proto/src
COPY ./drill-server/src ./drill-server/src
COPY ./drill-client/src ./drill-client/src

RUN source $HOME/.cargo/env && \
    cargo build --release --features dynamic --target="$RUST_TARGET" && \
    cp target/$RUST_TARGET/release/drill-server /drill-server && \
    strip /drill-server

FROM scratch

COPY --from=builder /drill-server /drill-server

ENTRYPOINT ["./drill-server"]
