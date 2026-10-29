interface Env {
    SNAPSHOTS: R2Bucket;
    SYNC_TOKEN: string;
}

const MAX_BYTES = 32 * 1024 * 1024;
const KEY = /^machines\/[a-fA-F0-9]{32}\.json$/;

function response(status: number, message: string): Response {
    return new Response(message, {
        status,
        headers: { "Cache-Control": "no-store" },
    });
}

async function authorized(request: Request, token: string): Promise<boolean> {
    if (!token) return false;

    const encode = new TextEncoder();
    const hash = (value: string) =>
        crypto.subtle.digest("SHA-256", encode.encode(value));
    const [actual, expected] = await Promise.all([
        hash(request.headers.get("Authorization") ?? ""),
        hash(`Bearer ${token}`),
    ]);

    return crypto.subtle.timingSafeEqual(actual, expected);
}

async function handle(request: Request, env: Env): Promise<Response> {
    if (!(await authorized(request, env.SYNC_TOKEN))) {
        return response(401, "Unauthorized");
    }

    const url = new URL(request.url);
    if (url.pathname === "/machines" && request.method === "GET") {
        const page = await env.SNAPSHOTS.list({
            prefix: "machines/",
            limit: 1000,
            cursor: url.searchParams.get("cursor") ?? undefined,
        });

        return Response.json(
            {
                objects: page.objects
                    .filter((object) => KEY.test(object.key))
                    .map((object) => ({
                        key: object.key,
                        etag: object.httpEtag,
                        size: object.size,
                        uploaded: object.uploaded.toISOString(),
                    })),
                cursor: page.truncated ? page.cursor : null,
            },
            { headers: { "Cache-Control": "no-store" } },
        );
    }

    const key = url.pathname.slice(1);
    if (!KEY.test(key)) return response(404, "Not found");

    if (request.method === "GET") {
        const object = await env.SNAPSHOTS.get(key);
        if (!object) return response(404, "Not found");

        return new Response(object.body, {
            headers: {
                "Content-Type": "application/json",
                "Content-Length": String(object.size),
                ETag: object.httpEtag,
                "Cache-Control": "no-store",
            },
        });
    }

    if (request.method !== "PUT") return response(405, "Method not allowed");

    const length = request.headers.get("Content-Length");
    if (length === null) return response(411, "Content-Length required");
    if (!/^\d+$/.test(length)) return response(400, "Invalid Content-Length");

    const size = Number(length);
    if (size > MAX_BYTES) return response(413, "Snapshot too large");
    if (size === 0 || !request.body) return response(400, "Empty snapshot");

    if (request.headers.get("Content-Type") !== "application/json") {
        return response(415, "Expected application/json");
    }

    const match = request.headers.get("If-Match");
    const create = request.headers.get("If-None-Match");
    if (
        !(
            (match && /^"[a-fA-F0-9]{32}"$/.test(match) && create === null) ||
            (match === null && create === "*")
        )
    ) {
        return response(
            428,
            "A single exact If-Match or If-None-Match: * is required",
        );
    }

    const conditions = new Headers();
    if (match) conditions.set("If-Match", match);
    else conditions.set("If-None-Match", "*");

    const object = await env.SNAPSHOTS.put(key, request.body, {
        onlyIf: conditions,
        httpMetadata: { contentType: "application/json" },
    });
    if (!object) return response(412, "Snapshot changed");

    return new Response(null, {
        status: 204,
        headers: { ETag: object.httpEtag, "Cache-Control": "no-store" },
    });
}

export default {
    async fetch(request: Request, env: Env): Promise<Response> {
        try {
            return await handle(request, env);
        } catch {
            return response(500, "Snapshot storage failed");
        }
    },
} satisfies ExportedHandler<Env>;
