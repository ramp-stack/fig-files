"""Publish only Fig Files invitation assets through the existing RAMP website."""
import hashlib
import json
from datetime import datetime, timezone
from pathlib import Path

import boto3
from botocore.exceptions import ClientError

ROOT = Path(__file__).resolve().parent
ACCOUNT = "173652135802"
BUCKET = "therampstack-website-173652135802"
DISTRIBUTION = "EKHQO7WBLP3XA"

if boto3.client("sts").get_caller_identity()["Account"] != ACCOUNT:
    raise SystemExit("Wrong AWS account; nothing uploaded.")
cloudfront = boto3.client("cloudfront")
config = cloudfront.get_distribution_config(Id=DISTRIBUTION)["DistributionConfig"]
if "therampstack.com" not in config["Aliases"].get("Items", []):
    raise SystemExit("Unexpected distribution; nothing uploaded.")

manifest = json.loads((ROOT / "manifest.json").read_text())
for key, expected in manifest.items():
    if hashlib.sha256((ROOT / key).read_bytes()).hexdigest() != expected:
        raise SystemExit(f"Checksum mismatch: {key}; nothing uploaded.")

s3 = boto3.client("s3", region_name="us-east-2")
association_keys = [
    ".well-known/apple-app-site-association",
    ".well-known/apple-app-site-association/index.html",
]
association = {}
for key in association_keys:
    try:
        existing = json.loads(s3.get_object(Bucket=BUCKET, Key=key)["Body"].read())
    except ClientError as error:
        if error.response["Error"]["Code"] in ("NoSuchKey", "404"):
            continue
        raise
    if not isinstance(existing, dict):
        raise SystemExit("Unexpected existing Apple association; nothing uploaded.")
    for section, value in existing.items():
        if section != "applinks":
            if section in association and association[section] != value:
                raise SystemExit("Conflicting Apple association sections; nothing uploaded.")
            association[section] = value
    target = association.setdefault("applinks", {"apps": [], "details": []})
    for detail in existing.get("applinks", {}).get("details", []):
        if detail not in target["details"]:
            target["details"].append(detail)
incoming = json.loads((ROOT / ".well-known/apple-app-site-association").read_text())
target = association.setdefault("applinks", {"apps": [], "details": []})
for detail in incoming["applinks"]["details"]:
    if detail not in target["details"]:
        target["details"].append(detail)
body = (json.dumps(association, indent=2) + "\n").encode()

uploaded = {}
for key, content_type in [
    ("fig-files/invite.js", "text/javascript"),
    ("fig-files/invite-icon.svg", "image/svg+xml"),
    ("fig-files/invite.html", "text/html; charset=utf-8"),
]:
    data = (ROOT / key).read_bytes()
    s3.put_object(Bucket=BUCKET, Key=key, Body=data, ContentType=content_type,
                  CacheControl="public, max-age=300")
    uploaded[key] = hashlib.sha256(data).hexdigest()
for key in association_keys:
    s3.put_object(Bucket=BUCKET, Key=key, Body=body, ContentType="application/json",
                  CacheControl="public, max-age=300")
    uploaded[key] = hashlib.sha256(body).hexdigest()

stamp = datetime.now(timezone.utc).isoformat()
paths = ["/" + key for key in uploaded]
invalidation = cloudfront.create_invalidation(
    DistributionId=DISTRIBUTION,
    InvalidationBatch={"Paths": {"Quantity": len(paths), "Items": paths}, "CallerReference": stamp},
)["Invalidation"]
receipt = {"published_at": stamp, "bucket": BUCKET, "distribution": DISTRIBUTION,
           "invalidation": invalidation["Id"], "files": uploaded}
(ROOT / "publish-receipt.json").write_text(json.dumps(receipt, indent=2) + "\n")
print(json.dumps(receipt, indent=2))
