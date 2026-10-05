"""Pinned source delivery, not legal approval. No upstream scripts are executed.

Stage accepts either network inputs or paired --local-gitbash/--local-native paths
(no filename convention). Local inputs must stay inside the repository; package,
output and work must stay under target/source-delivery-evaluation. Local staging
allows null source URLs but never changes publication gates or source validators.

Publish optionally accepts --variants int8 fp32. Stage the base package once,
then copy it to sibling packages (dist/neo-int8 and dist/neo-fp32 for dist/neo)
before packaging. Both variants must retain identical docs/licenses contents;
all four variant binaries and the two shared source ZIPs are required.
After final model preparation, run inventory --package dist/neo-int8 --variant
int8 --version VERSION (and likewise for fp32) before creating binary archives.
Inventory writes PACKAGE-MANIFEST.json; variant publication verifies the full tree.
"""
from __future__ import annotations

import argparse
import hashlib
import json
from pathlib import Path
import re
import shutil
import stat
import subprocess
import tempfile
import time
import urllib.parse
import urllib.request
import zipfile

if __package__:
    from . import assemble_drawing_release as drawing
    from . import check_distribution_review as distribution_review
    from . import native_source_bundle as native
    from . import native_source_binding as native_binding
    from . import prepare_runtime_distribution as runtime
    from .runtime_sources import check_path, file_record, load_json, safe_relative
else:
    import assemble_drawing_release as drawing
    import check_distribution_review as distribution_review
    import native_source_bundle as native
    import native_source_binding as native_binding
    import prepare_runtime_distribution as runtime
    from runtime_sources import check_path, file_record, load_json, safe_relative

ROOT = Path(__file__).resolve().parents[1]
LOCK = Path(__file__).with_name("source-companions.lock.json")
KINDS = {"gitbash", "native"}
VARIANTS = ("int8", "fp32")
PACKAGE_MANIFEST = "PACKAGE-MANIFEST.json"
CHUNK = 1024 * 1024


def require(condition, message):
    if not condition:
        raise ValueError(message)


def sha(value):
    require(isinstance(value, str) and re.fullmatch(r"[0-9a-f]{64}", value), "Invalid SHA-256 pin")


def https_url(value):
    require(isinstance(value, str) and not any(c.isspace() for c in value), "Expected fixed HTTPS source URL")
    url = urllib.parse.urlsplit(value)
    require(url.scheme == "https" and url.hostname and not url.username and not url.password
            and not url.fragment, "Expected fixed HTTPS source URL without credentials/fragment")
    return value


def read_lock(path, *, online=True):
    lock = load_json(path)
    require(lock.get("schema") == 1, "Unknown source lock schema")
    sources = lock["sources"]
    require(len(sources) == 2 and {s["kind"] for s in sources} == KINDS, "Expected native and gitbash sources exactly once")
    for source in sources:
        sha(source["sha256"])
        sha(source["expected_runtime_files_sha256"])
        require(type(source["size"]) is int and source["size"] > 0, "Invalid source size")
        if source["download_url"] is None:
            require(not online, f"BLOCKED: {source['kind']} download_url is null; reviewed, retrievable HTTPS source artifact required")
        else:
            https_url(source["download_url"])
    binding = next(s for s in sources if s["kind"] == "native")["native_binding"]
    if binding.get("kind") == "recipe":
        native_binding.check_binding(binding)
    else:
        sha(binding["manifest_sha256"])
        sha(binding["receipt_sha256"])
        require(binding == {"manifest_sha256": native.MANIFEST_SHA, "receipt_sha256": native.RECEIPT_SHA},
                "Native validator covers only the recorded local build; a new binding needs explicit review and validator support")
    return lock


def check_version(version):
    require(isinstance(version, str) and re.fullmatch(r"[0-9]+\.[0-9]+\.[0-9]+(?:-[A-Za-z0-9.-]+)?(?:\+[A-Za-z0-9.-]+)?", version), "Invalid version")


def identity(repository, tag, version):
    require(re.fullmatch(r"[A-Za-z0-9][A-Za-z0-9_.-]*/[A-Za-z0-9][A-Za-z0-9_.-]*", repository), "Invalid repository")
    check_version(version)
    require(tag == "v" + version, "Tag/version mismatch")


def asset_name(version, kind):
    require(kind in KINDS, "Unknown source kind")
    return f"neo-{version}-{kind}-sources.zip"


def release_url(repository, tag, name):
    return f"https://github.com/{repository}/releases/download/{urllib.parse.quote(tag, safe='')}/{urllib.parse.quote(name, safe='')}"


class HTTPSRedirects(urllib.request.HTTPRedirectHandler):
    def redirect_request(self, req, fp, code, msg, headers, newurl):
        https_url(newurl)
        return super().redirect_request(req, fp, code, msg, headers, newurl)


def download(source, destination, deadline):
    """No retries, credentials or cache fallback; bound bytes and wall-clock time."""
    https_url(source["download_url"])
    opener = urllib.request.build_opener(HTTPSRedirects())
    require(time.monotonic() < deadline, "Source download time budget exceeded")
    created = False
    try:
        check_path(destination)
        require(not destination.exists(), "Refusing to overwrite source download")
        request = urllib.request.Request(source["download_url"], headers={"Accept-Encoding": "identity"})
        with opener.open(request, timeout=min(30, deadline - time.monotonic())) as response, destination.open("xb") as output:
            created = True
            https_url(response.geturl())
            require(response.status == 200, "Source HTTP status is not 200")
            length = response.headers.get("Content-Length")
            require(length is None or int(length) == source["size"], "Source Content-Length mismatch")
            size, digest = 0, hashlib.sha256()
            while True:
                require(time.monotonic() < deadline, "Source download time budget exceeded")
                chunk = response.read(CHUNK)
                require(time.monotonic() < deadline, "Source download time budget exceeded")
                if not chunk:
                    break
                size += len(chunk)
                require(size <= source["size"], "Source download size budget exceeded")
                digest.update(chunk)
                output.write(chunk)
            require(size == source["size"] and digest.hexdigest() == source["sha256"], "Source download size/SHA-256 mismatch")
    except BaseException:
        if created:
            destination.unlink(missing_ok=True)
        raise


def safe_path(path, root):
    """Reject traversal, reparse points and paths outside the designated root."""
    check_path(path)
    check_path(root)
    require(path.absolute().is_relative_to(root.absolute()), f"Path must stay under {root}: {path}")
    safe_relative(path.absolute().relative_to(root.absolute()).as_posix())
    require(path.resolve().is_relative_to(root.resolve()), f"Path escapes {root}: {path}")
    return path


def tree_files(directory):
    check_path(directory)
    require(directory.is_dir(), f"Missing runtime directory: {directory}")
    files = []
    for path in sorted(directory.rglob("*")):
        check_path(path)
        if path.is_file():
            files.append({"path": safe_relative(path.relative_to(directory).as_posix()), **file_record(path)})
    runtime.records(files)
    require(files, "Empty runtime file set")
    return files


def package_manifest(package, variant, version):
    require(variant in VARIANTS, "Unknown package variant")
    check_version(version)
    check_path(package)
    require(package.is_dir(), f"Missing package directory: {package}")
    files = []

    def visit(directory):
        # Check before descending; never follow junctions or silently omit an
        # unreadable subtree. Only the root inventory is excluded, not namesakes.
        for path in sorted(directory.iterdir()):
            check_path(path)
            name = safe_relative(path.relative_to(package).as_posix())
            info = path.lstat()
            if stat.S_ISDIR(info.st_mode):
                require(name != PACKAGE_MANIFEST, "Package manifest must be a regular file")
                visit(path)
            else:
                require(stat.S_ISREG(info.st_mode) and info.st_nlink == 1,
                        f"Package entry must be a regular file without links: {name}")
                if name != PACKAGE_MANIFEST:
                    files.append({"path": name, **file_record(path)})

    visit(package)
    require(files, "Empty package file set")
    return {"schema": 1, "version": version, "variant": variant, "files": runtime.records(files)}


def inventory(package, variant, version):
    destination = package / PACKAGE_MANIFEST
    check_path(destination)
    require(not destination.exists(), "Refusing to overwrite package inventory")
    manifest = package_manifest(package, variant, version)
    created = False
    try:
        with destination.open("x", encoding="utf-8", newline="\n") as stream:
            created = True
            json.dump(manifest, stream, indent=2)
            stream.write("\n")
    except BaseException:
        if created:
            destination.unlink()
        raise
    return manifest


def verify_native_binding(source, package, native_lib, *, bundle=None, work=None):
    files = tree_files(package / "runtime/onnx")
    require(runtime.digest_json(files) == source["expected_runtime_files_sha256"], "Native runtime file set mismatch")
    binding = source["native_binding"]
    if binding.get("kind") == "recipe":
        require(bundle is not None and work is not None, "Recipe validation requires source ZIP and validation workspace")
        native_binding.verify(binding, bundle, native_lib, work)
        return
    for name, key in (("neo-sherpa-asr.json", "manifest_sha256"), ("neo-asr-receipt.json", "receipt_sha256")):
        require(file_record(native_lib / name)["sha256"] == binding[key],
                "BLOCKED: native build differs from source lock's recorded local build; not valid for arbitrary CI native builds")
    manifest = load_json(native_lib / "neo-sherpa-asr.json")
    require(manifest["status"] == "native-validated" and manifest["options"]["SHERPA_ONNX_ENABLE_TTS"] == "OFF", "Invalid native manifest")
    require(manifest["libraries"], "Empty native library binding")
    for name, expected in manifest["libraries"].items():
        require(file_record(native_lib / safe_relative(name)) == expected, "Native library differs from recorded build: " + name)


def verify_gitbash(bundle, source, distribution, packaged_runtime, work, max_expanded):
    current = runtime.verify_distribution(distribution, packaged_runtime)
    require(runtime.digest_json(current["runtime"]["files"]) == source["expected_runtime_files_sha256"], "Gitbash runtime file set mismatch")
    # Only metadata is materialized. Never extract or execute upstream source scripts.
    metadata = {"source-manifest.json": "source-manifest.json", "MODIFICATIONS.md": "MODIFICATIONS.md",
                "distribution/MANIFEST.json": "MANIFEST.json",
                "distribution/gitbash-distribution-policy.json": "gitbash-distribution-policy.json"}
    with zipfile.ZipFile(bundle) as archive:
        infos = archive.infolist()
        require(len(infos) <= 10000 and len({i.filename.casefold() for i in infos}) == len(infos), "Duplicate/member budget violation")
        require(sum(i.file_size for i in infos) <= max_expanded, "Expanded source budget exceeded")
        members = []
        for info in infos:
            safe_relative(info.filename)
            require(stat.S_IFMT(info.external_attr >> 16) in (0, stat.S_IFREG) and not info.is_dir(), "Non-regular source ZIP member")
            digest, size = hashlib.sha256(), 0
            with archive.open(info) as stream:
                while chunk := stream.read(CHUNK):
                    size += len(chunk)
                    require(size <= info.file_size, "Expanded member size mismatch")
                    digest.update(chunk)
            require(size == info.file_size, "Truncated source member")
            members.append({"path": info.filename, "size": size, "sha256": digest.hexdigest()})
            if info.filename in metadata:
                require(size <= 8 * CHUNK, "Source metadata exceeds budget")
                (work / metadata[info.filename]).write_bytes(archive.read(info))
    # The historical report includes smoke/provenance details that may differ on CI.
    # Validate its actual runtime file set rather than requiring CI report byte equality.
    historical = runtime.verify_distribution(work, packaged_runtime)
    require(historical["runtime"] == current["runtime"], "Historical companion/current runtime mismatch")
    # The existing companion validator uses a relative path in its metadata.
    # A temporary read-only-use hard link avoids copying a huge local ZIP. Never
    # fall back to copying or weakening validation if hard links are unavailable.
    linked = None
    try:
        local_bundle = bundle
        if bundle.parent != work:
            local_bundle = work / "validated-source.zip"
            check_path(local_bundle)
            local_bundle.hardlink_to(bundle)
            linked = local_bundle
        record = {"path": local_bundle.name, **file_record(bundle), "members": members}
        runtime.write_json(work / "source-companion-record.json", record)
        runtime.verify_companion(work, current)
    finally:
        if linked is not None:
            linked.unlink()
    source_manifest = load_json(work / "source-manifest.json")
    by_name = runtime.records(members)
    for artifact in source_manifest["artifacts"]:
        require(by_name.get("archives/" + safe_relative(artifact["path"])) ==
                {k: artifact[k] for k in ("size", "sha256")}, "Retained source artifact mismatch")


def verify_bundle(bundle, source, package, distribution, native_lib, work, max_expanded):
    require(file_record(bundle) == {k: source[k] for k in ("size", "sha256")}, "Source ZIP size/SHA-256 mismatch")
    if source["kind"] == "native":
        native.verify(bundle)
        verify_native_binding(source, package, native_lib, bundle=bundle, work=work)
    else:
        verify_gitbash(bundle, source, distribution, package / "runtime/gitbash", work, max_expanded)


def source_access(lock, repository, tag, version):
    identity(repository, tag, version)
    return {"schema": 1, "status": "planned-until-publication", "repository": repository, "tag": tag,
            "scope": "Source asset delivery only; not legal approval or proof of complete native source correspondence.",
            "sources": [{"kind": s["kind"], "name": asset_name(version, s["kind"]),
                         "sha256": s["sha256"], "size": s["size"],
                         "expected_runtime_files_sha256": s["expected_runtime_files_sha256"],
                         "url": release_url(repository, tag, asset_name(version, s["kind"]))}
                        for s in lock["sources"]]}


def stage(lock_path, package, distribution, native_lib, output, work_root, repository, tag, version,
          budget_seconds=600, max_download_mib=1024, max_expanded_mib=2048,
          *, local_gitbash=None, local_native=None):
    require((local_gitbash is None) == (local_native is None),
            "Local stage requires both --local-gitbash and --local-native")
    local = local_gitbash is not None
    local_sources = {"gitbash": local_gitbash, "native": local_native} if local else {}
    if local:
        evaluation = ROOT / "target/source-delivery-evaluation"
        for path in (package, output, work_root):
            safe_path(path, evaluation)
        for path in (lock_path, distribution, native_lib, *local_sources.values()):
            safe_path(path, ROOT)
        for path in local_sources.values():
            require(path.is_file(), f"Missing local source ZIP: {path}")
    lock = read_lock(lock_path, online=not local)
    notice = source_access(lock, repository, tag, version)
    require(budget_seconds > 0 and max_download_mib > 0 and max_expanded_mib > 0, "Budgets must be positive")
    require(sum(s["size"] for s in lock["sources"]) <= max_download_mib * CHUNK, "Aggregate source download budget exceeded")
    destinations = [output / s["name"] for s in notice["sources"]]
    notice_path = package / "docs/licenses/SOURCE-ACCESS.json"
    for path in (package, distribution, native_lib, output, work_root, notice_path, *destinations):
        check_path(path)
    require(output.is_dir() and package.is_dir() and notice_path.parent.is_dir(), "Missing package/output directories")
    require(not any(p.exists() for p in [notice_path, *destinations]), "Refusing to overwrite source delivery output")
    work_root.mkdir(parents=True, exist_ok=True)
    deadline = time.monotonic() + budget_seconds
    with tempfile.TemporaryDirectory(prefix="source-delivery-", dir=work_root) as temporary:
        work = Path(temporary)
        bundles = []
        for source in lock["sources"]:
            directory = work / source["kind"]
            directory.mkdir()
            if local:
                bundle = local_sources[source["kind"]]
            else:
                bundle = directory / asset_name(version, source["kind"])
                download(source, bundle, deadline)
            verify_bundle(bundle, source, package, distribution, native_lib, directory, max_expanded_mib * CHUNK)
            bundles.append(bundle)
        created = []
        try:
            for bundle, destination, source in zip(bundles, destinations, lock["sources"]):
                with destination.open("xb") as out:
                    created.append(destination)
                    with bundle.open("rb") as inp:
                        shutil.copyfileobj(inp, out, CHUNK)
                require(file_record(destination) == {k: source[k] for k in ("size", "sha256")}, "Staged source changed")
            with notice_path.open("x", encoding="utf-8", newline="\n") as stream:
                created.append(notice_path)
                json.dump(notice, stream, indent=2)
                stream.write("\n")
        except BaseException:
            for path in created:
                path.unlink()
            raise
    return notice


def run_gh(arguments):
    return subprocess.run(["gh", *arguments], check=True, capture_output=True, text=True,
                          encoding="utf-8", timeout=600).stdout


def verify_remote(assets, expected):
    require(len(assets) == len(expected) and len({a["name"] for a in assets}) == len(assets), "Remote asset count/duplicate mismatch")
    require({a["name"] for a in assets} == set(expected), "Remote asset names mismatch")
    for asset in assets:
        record = expected[asset["name"]]
        require(asset.get("state") == "uploaded" and asset.get("size") == record["size"], "Remote asset state/size mismatch: " + asset["name"])
        # Fail closed even on API versions that omit digest; do not redownload huge sources.
        require(asset.get("digest") == "sha256:" + record["sha256"], "Remote SHA-256 missing/mismatch: " + asset["name"])


def check_publication_review(repository, tag, version):
    """Enforce workflow gates for direct callers too, without any network calls."""
    record = distribution_review.check_review(ROOT / distribution_review.POLICY)
    drawing.approval(drawing.load_lock())
    identity(repository, tag, version)
    require(version == record["review"]["root_version"], "Release version differs from reviewed root version")
    # check_review binds HEAD's source tree to the reviewed commit, allowing only
    # the approval-record commit itself to differ. The release tag must name HEAD.
    tag_commit = distribution_review.git(ROOT, "rev-parse", "--verify", f"refs/tags/{tag}^{{commit}}").strip()
    head = distribution_review.git(ROOT, "rev-parse", "--verify", "HEAD").strip()
    require(re.fullmatch(rb"[0-9a-f]{40}", head) and tag_commit == head,
            "Existing local release tag must point at the reviewed HEAD commit")


def publish(lock_path, package, output, repository, tag, version, notes, prerelease, *, variants=None):
    """Explicit gated network command. Failure leaves the draft private."""
    check_publication_review(repository, tag, version)
    if variants is not None:
        require(len(variants) == len(VARIANTS) and set(variants) == set(VARIANTS),
                "Variants must include int8 and fp32 exactly once, with no unknown variants")
    lock = read_lock(lock_path)
    expected_notice = source_access(lock, repository, tag, version)
    require(load_json(package / "docs/licenses/SOURCE-ACCESS.json") == expected_notice, "Source access notice differs from lock/release identity")
    if variants is not None:
        # SOURCE-ACCESS.json is the staged delivery manifest; compare bytes too,
        # together with the other source/legal notices copied from the base.
        base_notices = tree_files(package / "docs/licenses")
        for variant in VARIANTS:
            variant_package = package.with_name(f"{package.name}-{variant}")
            require(load_json(variant_package / "docs/licenses/SOURCE-ACCESS.json") == expected_notice,
                    f"Variant {variant} source access notice differs from base/lock/release identity")
            require(tree_files(variant_package / "docs/licenses") == base_notices,
                    f"Variant {variant} source/legal notices differ from base package")
            manifest = load_json(variant_package / PACKAGE_MANIFEST)
            require(manifest == package_manifest(variant_package, variant, version),
                    f"Variant {variant} package manifest differs from current tree/version/variant")
    require(prerelease == ("-" in version.split("+", 1)[0]), "Prerelease/version mismatch")
    paths, expected = [], {}
    for source in expected_notice["sources"]:
        path = output / source["name"]
        record = file_record(path)
        require(record == {k: source[k] for k in ("size", "sha256")}, "Source asset missing/changed before upload")
        paths.append(path)
        expected[path.name] = record
    prefixes = [f"neo-{version}-{variant}" for variant in VARIANTS] if variants is not None else [f"neo-{version}"]
    for prefix in prefixes:
        for suffix in ("portable-x64.zip", "installer-x64.exe"):
            path = output / f"{prefix}-{suffix}"
            record = file_record(path)
            require(record["size"] > 0, "Empty binary asset")
            paths.append(path)
            expected[path.name] = record
    check_path(notes)
    require(notes.is_file(), "Missing release notes")
    # All assets in the same create invocation; never expose an incomplete release.
    args = ["release", "create", tag, *(str(p) for p in paths), "--repo", repository,
            "--draft", "--verify-tag", "--title", "Neo " + tag, "--notes-file", str(notes)]
    if prerelease:
        args.append("--prerelease")
    run_gh(args)
    release = json.loads(run_gh(["release", "view", tag, "--repo", repository,
                                "--json", "databaseId,tagName,isDraft,isPrerelease"]))
    require(release["isDraft"] is True and release["tagName"] == tag and release["isPrerelease"] == prerelease,
            "Expected matching private draft before asset verification")
    release_id = release["databaseId"]
    require(type(release_id) is int and release_id > 0, "Invalid release ID")
    pages = json.loads(run_gh(["api", "--paginate", "--slurp", "-H", "X-GitHub-Api-Version: 2022-11-28",
                              f"repos/{repository}/releases/{release_id}/assets?per_page=100"]))
    verify_remote([asset for page in pages for asset in page], expected)
    for path in paths:
        require(file_record(path) == expected[path.name], "Local asset changed during upload")
    run_gh(["release", "edit", tag, "--repo", repository, "--draft=false",
            "--prerelease=" + str(prerelease).lower()])


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("command", choices=("stage", "publish", "inventory"))
    parser.add_argument("--lock", type=Path, default=LOCK)
    parser.add_argument("--package", type=Path, required=True)
    parser.add_argument("--output", type=Path)
    parser.add_argument("--repository")
    parser.add_argument("--tag")
    parser.add_argument("--version", required=True)
    parser.add_argument("--distribution", type=Path)
    parser.add_argument("--native-lib", type=Path)
    parser.add_argument("--work-root", type=Path, help="Default: target/source-delivery (local: target/source-delivery-evaluation/work)")
    parser.add_argument("--local-gitbash", type=Path, help="Stage only; existing ZIP inside repository, requires --local-native")
    parser.add_argument("--local-native", type=Path, help="Stage only; existing ZIP inside repository, requires --local-gitbash")
    parser.add_argument("--budget-seconds", type=int, default=600)
    parser.add_argument("--max-download-mib", type=int, default=1024)
    parser.add_argument("--max-expanded-mib", type=int, default=2048)
    parser.add_argument("--notes", type=Path)
    parser.add_argument("--variant", choices=VARIANTS, help="Inventory only; package math variant")
    parser.add_argument("--variants", nargs="+", choices=VARIANTS,
                        help="Publish only; requires both int8 and fp32 and their sibling packages (default: single package)")
    parser.add_argument("--prerelease", choices=("true", "false"), default="false")
    args = parser.parse_args()
    try:
        require((args.local_gitbash is None) == (args.local_native is None),
                "Local stage requires both --local-gitbash and --local-native")
        local = args.local_gitbash is not None
        require(not local or args.command == "stage", "Local sources are stage-only; publication gates are unchanged")
        require(args.variants is None or args.command == "publish", "Variants are publish-only; stage the base package once")
        require(args.variant is None or args.command == "inventory", "--variant is inventory-only")
        if args.command == "inventory":
            require(args.variant is not None, "Inventory requires --variant")
            inventory(args.package, args.variant, args.version)
            print("Package inventory written; not legal approval")
            return
        require(args.output is not None and args.repository is not None and args.tag is not None,
                "Stage/publish requires --output, --repository and --tag")
        if args.command == "stage":
            if args.work_root is None:
                args.work_root = ROOT / ("target/source-delivery-evaluation/work" if local else "target/source-delivery")
            require(args.distribution is not None and args.native_lib is not None, "Stage requires --distribution and --native-lib")
            require(args.work_root.resolve().is_relative_to((ROOT / "target").resolve()), "Temporary work must stay under target")
            stage(args.lock, args.package, args.distribution, args.native_lib, args.output, args.work_root,
                  args.repository, args.tag, args.version, args.budget_seconds, args.max_download_mib, args.max_expanded_mib,
                  local_gitbash=args.local_gitbash, local_native=args.local_native)
            print("Source delivery staged; planned until publication, not legal approval")
        else:
            require(args.notes is not None, "Publish requires --notes")
            publish(args.lock, args.package, args.output, args.repository, args.tag, args.version, args.notes, args.prerelease == "true",
                    variants=args.variants)
    except (ValueError, OSError, KeyError, TypeError, zipfile.BadZipFile, subprocess.SubprocessError) as error:
        parser.exit(1, f"source delivery BLOCKED: {error}\n")


if __name__ == "__main__":
    main()
