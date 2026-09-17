FROM alpine@sha256:28bd5fe8b56d1bd048e5babf5b10710ebe0bae67db86916198a6eec434943f8b
RUN apk add --no-cache busybox-extras && adduser -D -u 1000 engineer && mkdir -p /opt/steward-fixture
COPY start.sh health.sh restore-api-write.sh /opt/steward-fixture/
RUN chmod 755 /opt/steward-fixture/*.sh
USER 1000:1000
ENTRYPOINT ["/opt/steward-fixture/start.sh"]
