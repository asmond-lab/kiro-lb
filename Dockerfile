FROM alpine:3.22 AS pick
ARG TARGETARCH
COPY dist/ /dist/
RUN if [ "$TARGETARCH" = "arm64" ]; then cp /dist/kirolb-linux-arm64 /kirolb; else cp /dist/kirolb-linux-x64 /kirolb; fi && chmod 0755 /kirolb

FROM alpine:3.22
RUN apk add --no-cache ca-certificates wget && adduser -D -u 1000 kirolb && mkdir -p /app/data /app/debug_logs && chown -R kirolb /app
WORKDIR /app
COPY --from=pick /kirolb /usr/local/bin/kirolb
USER kirolb
ENV SERVER_HOST=0.0.0.0 SERVER_PORT=8000
EXPOSE 8000
HEALTHCHECK --interval=30s --timeout=5s --start-period=5s --retries=3 CMD wget -q -O /dev/null http://127.0.0.1:8000/health || exit 1
ENTRYPOINT ["/usr/local/bin/kirolb"]
