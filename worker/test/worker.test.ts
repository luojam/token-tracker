import assert from "node:assert/strict";
import { execFile } from "node:child_process";
import { mkdir, mkdtemp, readFile, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
import { after, before, test } from "node:test";
import { promisify } from "node:util";
import { build } from "esbuild";
import { Miniflare } from "miniflare";

let runtime: Miniflare;
const key = `machines/${"a".repeat(32)}.json`;
const token = "test-secret";

async function createRuntime() {
    const result = await build({
        entryPoints: ["src/index.ts"],
        bundle: true,
        write: false,
        format: "esm",
    });

    return new Miniflare({
        telemetry: { enabled: false },
        workers: [
            {
                config: {
                    name: "test",
                    compatibilityDate: "2026-10-23",
                    manifest: {
                        mainModule: "index.js",
                        modules: {
                            "index.js": {
                                type: "esm",
                                contents: result.outputFiles[0].text,
                            },
                        },
                    },
                    env: {
                        SNAPSHOTS: { type: "r2", name: "test-snapshots" },
                        SYNC_TOKEN: { type: "text", value: token },
                    },
                },
            },
        ],
    });
}

before(async () => {
    runtime = await createRuntime();
});

after(async () => {
    await runtime?.dispose();
});

function request(
    path: string,
    method = "GET",
    body?: string,
    headers: Record<string, string> = {},
) {
    return runtime.dispatchFetch(`http://local/${path}`, {
        method,
        body,
        headers: {
            Authorization: `Bearer ${token}`,
            ...(body
                ? {
                      "Content-Type": "application/json",
                      "Content-Length": String(Buffer.byteLength(body)),
                  }
                : {}),
            ...headers,
        },
    });
}

test("authentication, conditional replacement, and streaming downloads", async () => {
    assert.equal(
        (
            await request("machines", "GET", undefined, {
                Authorization: "Bearer wrong",
            })
        ).status,
        401,
    );
    assert.equal((await request("machines/invalid.json")).status, 404);
    assert.equal((await request(key, "PUT", "{}")).status, 428);

    const first = await request(key, "PUT", '{"rows":[1]}', {
        "If-None-Match": "*",
    });
    assert.equal(first.status, 204);
    const etag = first.headers.get("ETag");
    assert.ok(etag);

    assert.equal(
        (await request(key, "PUT", "{}", { "If-None-Match": "*" })).status,
        412,
    );

    const next = await request(key, "PUT", '{"rows":[2]}', {
        "If-Match": etag,
    });
    assert.equal(next.status, 204);

    assert.equal(
        (await request(key, "PUT", '{"rows":[3]}', { "If-Match": etag }))
            .status,
        412,
    );

    const download = await request(key);
    assert.equal(await download.text(), '{"rows":[2]}');
    assert.equal(download.headers.get("ETag"), next.headers.get("ETag"));

    const listing = (await (await request("machines")).json()) as {
        objects: {
            key: string;
            etag: string;
            size: number;
            uploaded: string;
        }[];
        cursor: string | null;
    };
    assert.equal(listing.objects.length, 1);
    assert.equal(listing.objects[0].key, key);
    assert.equal(listing.objects[0].etag, next.headers.get("ETag"));
    assert.equal(listing.objects[0].size, 12);
    assert.ok(Date.parse(listing.objects[0].uploaded));
    assert.equal(listing.cursor, null);
});

test("streamed transfers enforce the size limit without damaging stored data", async () => {
    const transferKey = `machines/${"b".repeat(32)}.json`;
    const body = JSON.stringify({ padding: "x".repeat(1024 * 1024) });

    assert.equal(
        (await request(transferKey, "PUT", body, { "If-None-Match": "*" }))
            .status,
        204,
    );

    const large = " ".repeat(32 * 1024 * 1024 + 1);
    const result = await request(transferKey, "PUT", large, {
        "If-None-Match": "*",
    });
    assert.equal(result.status, 413);

    assert.equal(await (await request(transferKey)).text(), body);

    await (await runtime.getR2Bucket("SNAPSHOTS")).delete(transferKey);
});

test("list pages include every machine", async () => {
    const bucket = await runtime.getR2Bucket("SNAPSHOTS");
    await Promise.all(
        Array.from({ length: 1001 }, (_, n) =>
            bucket.put(
                `machines/${n.toString(16).padStart(32, "0")}.json`,
                "{}",
            ),
        ),
    );

    let cursor: string | null = null;
    const keys = new Set<string>();

    do {
        const page = (await (
            await request(
                `machines${cursor ? `?cursor=${encodeURIComponent(cursor)}` : ""}`,
            )
        ).json()) as { objects: { key: string }[]; cursor: string | null };

        for (const object of page.objects) {
            assert.ok(!keys.has(object.key));
            keys.add(object.key);
        }

        cursor = page.cursor;
    } while (cursor);

    assert.equal(keys.size, 1002);
});

test("CLI sync and remote reports round-trip through the Worker", async () => {
    const exec = promisify(execFile);
    const root = resolve("..");
    await exec("cargo", ["build", "--quiet"], { cwd: root });

    const directory = await mkdtemp(join(tmpdir(), "token-tracker-remote-"));
    const server = await createRuntime();

    try {
        const origin = (await server.ready).origin;
        const config = join(directory, "config/token-tracker");
        const sessions = join(directory, "pi");

        await mkdir(config, { recursive: true });
        await mkdir(sessions);
        await writeFile(
            join(config, "config.toml"),
            `remote_url = "${origin}"\nmachine_name = "Test laptop"\n`,
        );
        await writeFile(
            join(config, "auth.toml"),
            `remote_token = "${token}"\n`,
        );

        const fixture = await readFile(
            join(root, "tests/fixtures/pi/all-usage.jsonl"),
            "utf8",
        );
        await writeFile(join(sessions, "session.jsonl"), fixture);

        const env = {
            ...process.env,
            HOME: directory,
            XDG_CONFIG_HOME: join(directory, "config"),
            XDG_DATA_HOME: join(directory, "data"),
            PI_CODING_AGENT_SESSION_DIR: sessions,
            CODEX_HOME: join(directory, "codex"),
            CLAUDE_CONFIG_DIR: join(directory, "claude"),
            HERMES_HOME: join(directory, "hermes"),
        };
        const run = (args: string[]) =>
            exec(join(root, "target/debug/token-tracker"), args, {
                env,
                cwd: directory,
            });

        const sync = await run(["sync"]);
        assert.match(
            sync.stdout,
            /Synced [a-f0-9]{32}: \d+ daily rows, \d+ bytes/,
        );

        const uploaded = await run(["--remote"]);
        assert.match(uploaded.stdout, /Input tokens: 25\n/);
        assert.match(uploaded.stdout, /Test laptop/);
        assert.match(uploaded.stdout, /old\)/);
        assert.doesNotMatch(
            uploaded.stdout,
            /Sessions:|Unique usage events:|SECRET_|\/work\//,
        );

        await writeFile(
            join(sessions, "session.jsonl"),
            fixture.replace('"input":10', '"input":1000'),
        );
        assert.match((await run(["--remote"])).stdout, /Input tokens: 25\n/);

        await run(["sync"]);
        assert.match(
            (await run(["summary", "--remote"])).stdout,
            /Input tokens: 1,015\n/,
        );

        assert.match(
            (await run(["--remote", "--provider", "provider-a"])).stdout,
            /Input tokens: 1,000\n/,
        );

        await server.dispose();
        await assert.rejects(run(["--remote"]));
        assert.match((await run(["summary"])).stdout, /Input tokens: 1,015\n/);
    } finally {
        await server.dispose();
        await rm(directory, { recursive: true, force: true });
    }
});
