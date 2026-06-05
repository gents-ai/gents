FROM python:3.12-slim

WORKDIR /fixture

COPY requirements.txt .
RUN python -m pip install --no-cache-dir -r requirements.txt

COPY generate_fixture.py .

ENV OTEL_SDK_DISABLED=true
ENV DEFRA_FIXTURE_CAPTURE=docker
ENTRYPOINT ["python", "/fixture/generate_fixture.py"]
CMD ["--out-dir", "/out"]
