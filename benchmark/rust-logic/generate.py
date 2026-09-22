"""Generate the Rust concurrency-logic benchmark.

Each task is a Chinese prompt (what the model sees), a frozen ConcIR
contract, a reference lowering that must PASS, and a buggy lowering that
must FAIL. Rust guard drops are explicit mutex_unlock statements.
"""

from __future__ import annotations

import json
from pathlib import Path

ROOT = Path(__file__).resolve().parent
TASKS = ROOT / "tasks"

BOUNDS = {
    "max_threads": 8,
    "max_frames_per_thread": 8,
    "max_states": 20000,
    "max_depth": 200,
    "max_boundary_events": 256,
}


def dump(path: Path, value) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(json.dumps(value, indent=2, ensure_ascii=False) + "\n", encoding="utf-8")


def mutex(name: str) -> dict:
    return {"name": name, "kind": "sync", "type": "Mutex", "mode": "Sync"}


def condvar(name: str) -> dict:
    return {"name": name, "kind": "sync", "type": "Condvar", "mode": "Sync"}


def channel(name: str, capacity: int) -> dict:
    return {
        "name": name,
        "kind": "sync",
        "type": "Channel",
        "mode": "Sync",
        "base": "Int",
        "capacity": capacity,
    }


def semaphore(name: str, count: int) -> dict:
    return {
        "name": name,
        "kind": "sync",
        "type": "Semaphore",
        "mode": "Sync",
        "count": count,
    }


def var_int(name: str, hi: int) -> dict:
    return {
        "name": name,
        "kind": "var",
        "type": "Var",
        "base": {"Int": [0, hi]},
        "init": 0,
    }


def var_bool(name: str) -> dict:
    return {"name": name, "kind": "var", "type": "Var", "base": "Bool", "init": False}


def program(name: str, resources: list, functions: list, protection: list) -> dict:
    return {
        "program": name,
        "version": "3.5.0",
        "modules": [
            {
                "name": "main",
                "provides": {
                    "resources": [r["name"] for r in resources],
                    "functions": [f["name"] for f in functions],
                },
                "requires": {"resources": [], "functions": []},
                "resources": resources,
                "protection": protection,
                "functions": functions,
            }
        ],
        "entry": "main::main",
    }


def fn_main(workers: list[str]) -> dict:
    return {
        "name": "main",
        "kind": "normal",
        "body": [
            {"sid": "s1", "kind": "scope", "funcs": workers},
            {"sid": "s2", "kind": "return"},
        ],
    }


def fn_worker(name: str, body: list, locals: list | None = None) -> dict:
    function = {"name": name, "kind": "normal", "form": "closure", "body": body}
    if locals:
        function["locals"] = locals
    return function


def fn_plain(name: str, body: list) -> dict:
    return {"name": name, "kind": "normal", "body": body}


def stmt(sid: str, kind: str, **fields) -> dict:
    return {"sid": sid, "kind": kind, **fields}


def contract(name: str, properties: list) -> dict:
    return {
        "name": name,
        "properties": properties,
        "preserved": [],
        "assumptions": {
            "sequential_consistency": True,
            "no_spurious_wakeups": True,
        },
        "bounds": BOUNDS,
        "allowed_scope": {"allow_lock_reorder": False, "allow_statement_delete": False},
    }


def done(functions: list[str]) -> dict:
    return {
        "kind": "and",
        "predicates": [
            {"kind": "function_completed", "function": f"main::{name}"} for name in functions
        ],
    }


def props_finish(workers: list[str], extra: list | None = None) -> list:
    properties = [
        {"kind": "deadlock_free", "id": "no-deadlock"},
        {"kind": "reachability", "id": "workers-finish", "goal": done(workers)},
    ]
    if extra:
        properties.extend(extra)
    return properties


def bad_final(workers: list[str], resource: str, expected: int) -> dict:
    return {
        "kind": "unreachable",
        "id": "bad-final",
        "bad": {
            "kind": "and",
            "predicates": [
                *done(workers)["predicates"],
                {
                    "kind": "not",
                    "predicate": {
                        "kind": "var_eq",
                        "resource": resource,
                        "value": expected,
                    },
                },
            ],
        },
    }


def lock_body(order: list[str], resource: str, delta: int) -> list:
    first, second = order
    return [
        stmt("s1", "mutex_lock", resource=first),
        stmt("s2", "mutex_lock", resource=second),
        stmt("s3", "write_shared", resource=resource, expr=f"{resource} + {delta}"),
        stmt("s4", "mutex_unlock", resource=second),
        stmt("s5", "mutex_unlock", resource=first),
        stmt("s6", "return"),
    ]


def flag_body(order: list[str], resource: str) -> list:
    first, second = order
    return [
        stmt("s1", "mutex_lock", resource=first),
        stmt("s2", "mutex_lock", resource=second),
        stmt("s3", "write_shared", resource=resource, expr="true"),
        stmt("s4", "mutex_unlock", resource=second),
        stmt("s5", "mutex_unlock", resource=first),
        stmt("s6", "return"),
    ]


def critical_inc(lock: str, resource: str, delta: int) -> list:
    return [
        stmt("s1", "mutex_lock", resource=lock),
        stmt("s2", "write_shared", resource=resource, expr=f"{resource} + {delta}"),
        stmt("s3", "mutex_unlock", resource=lock),
        stmt("s4", "return"),
    ]


def split_inc(lock: str, resource: str, delta: int) -> list:
    return [
        stmt("s1", "mutex_lock", resource=lock),
        stmt("s2", "read_shared", resource=resource, dst="tmp"),
        stmt("s3", "mutex_unlock", resource=lock),
        stmt("s4", "mutex_lock", resource=lock),
        stmt("s5", "write_shared", resource=resource, expr=f"tmp + {delta}"),
        stmt("s6", "mutex_unlock", resource=lock),
        stmt("s7", "return"),
    ]


def cond_producer(lock: str, cv: str, resource: str, expr: str, notify: str) -> list:
    return [
        stmt("s1", "mutex_lock", resource=lock),
        stmt("s2", "write_shared", resource=resource, expr=expr),
        stmt("s3", notify, condvar=cv),
        stmt("s4", "mutex_unlock", resource=lock),
        stmt("s5", "return"),
    ]


def cond_consumer_ok(lock: str, cv: str, cond: str) -> list:
    return [
        stmt("s1", "mutex_lock", resource=lock),
        stmt("s2", "branch", cond=cond, then="s5", **{"else": "s3"}),
        stmt("s3", "condvar_wait", condvar=cv, lock=lock),
        stmt("s4", "goto", target="s2"),
        stmt("s5", "mutex_unlock", resource=lock),
        stmt("s6", "return"),
    ]


def cond_consumer_bug(lock: str, cv: str) -> list:
    return [
        stmt("s1", "mutex_lock", resource=lock),
        stmt("s2", "condvar_wait", condvar=cv, lock=lock),
        stmt("s3", "mutex_unlock", resource=lock),
        stmt("s4", "return"),
    ]


def send_body(ch: str, values: list[int]) -> list:
    body = [
        stmt(f"s{i + 1}", "channel_send", channel=ch, value=str(value))
        for i, value in enumerate(values)
    ]
    body.append(stmt(f"s{len(values) + 1}", "return"))
    return body


def recv_body(ch: str, n: int) -> list:
    body = [stmt(f"s{i + 1}", "channel_recv", channel=ch, dst="msg") for i in range(n)]
    body.append(stmt(f"s{n + 1}", "return"))
    return body


def sem_body(name: str, acquires: int, releases: int) -> list:
    body = []
    sid = 1
    for _ in range(acquires):
        body.append(stmt(f"s{sid}", "semaphore_acquire", resource=name))
        sid += 1
    for _ in range(releases):
        body.append(stmt(f"s{sid}", "semaphore_release", resource=name))
        sid += 1
    body.append(stmt(f"s{sid}", "return"))
    return body


def write_task(task: dict) -> None:
    directory = TASKS / task["id"]
    dump(directory / "contract.json", task["contract"])
    dump(directory / "reference.json", task["reference"])
    dump(directory / "bug.json", task["bug"])
    (directory / "prompt.md").write_text(task["prompt"], encoding="utf-8")


def prompt(title: str, scene: str, lines: list[str], done_lines: list[str]) -> str:
    body = "\n".join(f"- {line}" for line in lines)
    done_body = "\n".join(f"- {line}" for line in done_lines)
    return (
        f"# {title}\n\n"
        "用 safe Rust 写一个小程序，只使用标准库的 `std::thread` 和 `std::sync`。\n\n"
        f"## 场景\n\n{scene}\n\n"
        "## 约束\n\n"
        f"{body}\n"
        "- 不要使用 `unsafe`、`async`/`await`、`RwLock`、`Once`，也不要使用无界的 `mpsc::channel`。有界通信用 `mpsc::sync_channel`。\n"
        "- 锁守卫离开作用域就释放。不要在仍持有某把 `Mutex` 时再次获取它。\n"
        "- 两个工作线程用 `thread::scope` 启动，并在入口线程里等待它们结束。\n\n"
        "## 完成标准\n\n"
        f"{done_body}\n"
    )


def build() -> list[dict]:
    tasks = []
    tasks.extend(lock_order_tasks())
    tasks.extend(double_lock_tasks())
    tasks.extend(condvar_tasks())
    tasks.extend(channel_tasks())
    tasks.extend(atomicity_tasks())
    tasks.extend(semaphore_tasks())
    return tasks


def lock_order_tasks() -> list[dict]:
    specs = [
        ("01", "账本", "first", "second", "balance", [1, 1], 2, ["first", "second"]),
        ("02", "库存", "outer", "inner", "stock", [1, 3], 4, ["outer", "inner"]),
        ("03", "会话", "gate", "book", "ready", None, None, ["gate", "book"]),
        ("04", "转账", "src", "dst", "total", [2, 2], 4, ["src", "dst"]),
        ("05", "配额", "m1", "m2", "used", [1, 4], 5, ["m1", "m2"]),
    ]
    out = []
    for index, scene, a, b, resource, deltas, expected, order in specs:
        workers = ["left", "right"]
        bug_order = list(reversed(order))
        if deltas is None:
            resources = [mutex(a), mutex(b), var_bool(resource)]
            protection = [{"var": resource, "lock": a}]
            left = flag_body(order, resource)
            right = flag_body(order, resource)
            bad_right = flag_body(bug_order, resource)
            extra = [
                {
                    "kind": "reachability",
                    "id": "flag-set",
                    "goal": {
                        "kind": "and",
                        "predicates": [
                            *done(workers)["predicates"],
                            {"kind": "var_eq", "resource": resource, "value": True},
                        ],
                    },
                }
            ]
            done_lines = [
                "不会死锁。",
                "两个工作线程都返回。",
                f"共享标志 `{resource}` 最终为真。",
            ]
            detail = f"两个线程都把 `{resource}` 写成真。写的时候必须同时持有 `{a}` 和 `{b}`，并且先获取 `{order[0]}`，再获取 `{order[1]}`。"
        else:
            hi = expected
            resources = [mutex(a), mutex(b), var_int(resource, hi)]
            protection = [{"var": resource, "lock": a}]
            left = lock_body(order, resource, deltas[0])
            right = lock_body(order, resource, deltas[1])
            bad_right = lock_body(bug_order, resource, deltas[1])
            extra = [bad_final(workers, resource, expected)]
            done_lines = [
                "不会死锁。",
                "两个工作线程都返回。",
                f"`{resource}` 在两边都返回后等于 {expected}。",
            ]
            detail = (
                f"`left` 把 `{resource}` 增加 {deltas[0]}，`right` 增加 {deltas[1]}。"
                f"更新时必须同时持有 `{a}` 和 `{b}`，并且两个线程都先获取 `{order[0]}`，再获取 `{order[1]}`。"
            )
        ident = f"lock_order_{index}"
        functions_ok = [fn_main(workers), fn_worker("left", left), fn_worker("right", right)]
        functions_bad = [fn_main(workers), fn_worker("left", left), fn_worker("right", bad_right)]
        out.append(
            {
                "id": ident,
                "family": "lock_order",
                "title": f"{scene}：两把锁同一顺序",
                "reference_outcome": "PASS",
                "bug_outcome": "FAIL",
                "prompt": prompt(
                    f"{scene}：两把锁同一顺序",
                    f"这是一个{scene}更新。{detail}",
                    [
                        "恰好两个工作线程，分别叫 `left` 和 `right`。",
                        "每把锁只表示协议顺序，数据放在单独的共享单元里。",
                    ],
                    done_lines,
                ),
                "contract": contract(ident, props_finish(workers, extra)),
                "reference": program(ident, resources, functions_ok, protection),
                "bug": program(ident + "_bug", resources, functions_bad, protection),
            }
        )
    return out


def double_lock_tasks() -> list[dict]:
    specs = [
        ("01", "缓存", "guard", "slot", "owner", "peer", "touch"),
        ("02", "连接", "client", "state", "open", "watch", "refresh"),
        ("03", "文档", "doc", "rev", "editor", "reader", "save"),
        ("04", "队列", "head", "len", "push", "pop", "fix"),
        ("05", "配置", "cfg", "gen", "apply", "check", "edit"),
    ]
    out = []
    for index, scene, lock, resource, owner, peer, helper in specs:
        workers = [owner, peer]
        resources = [mutex(lock), var_int(resource, 3)]
        protection = [{"var": resource, "lock": lock}]
        owner_ok = [
            stmt("s1", "mutex_lock", resource=lock),
            stmt("s2", "write_shared", resource=resource, expr=f"{resource} + 1"),
            stmt("s3", "mutex_unlock", resource=lock),
            stmt("s4", "call", func=helper, args=[]),
            stmt("s5", "return"),
        ]
        owner_bad = [
            stmt("s1", "mutex_lock", resource=lock),
            stmt("s2", "call", func=helper, args=[]),
            stmt("s3", "mutex_unlock", resource=lock),
            stmt("s4", "return"),
        ]
        helper_body = critical_inc(lock, resource, 1)
        peer_body = critical_inc(lock, resource, 1)
        ident = f"double_lock_{index}"
        text = prompt(
            f"{scene}：调用期间不要持有同一把锁",
            (
                f"`{owner}` 先把 `{resource}` 加 1，然后调用 `{helper}`。"
                f"`{helper}` 自己再获取 `{lock}`，把 `{resource}` 再加 1。"
                f"`{peer}` 也在 `{lock}` 下把 `{resource}` 加 1。"
                f"三个加一都发生后，`{resource}` 等于 3。"
                f"`{owner}` 调用 `{helper}` 时必须已经释放 `{lock}`，因为 Rust 的 `Mutex` 不可重入。"
            ),
            [
                f"工作线程是 `{owner}` 和 `{peer}`。`{helper}` 是普通函数，不是第三个线程。",
                f"共享单元 `{resource}` 只由 `{lock}` 保护。",
            ],
            [
                "不会死锁。",
                f"`{owner}` 和 `{peer}` 都返回。",
                f"`{resource}` 在两边都返回后等于 3。",
            ],
        )
        extra = [bad_final(workers, resource, 3)]
        out.append(
            {
                "id": ident,
                "family": "double_lock",
                "title": f"{scene}：调用期间不要持有同一把锁",
                "reference_outcome": "PASS",
                "bug_outcome": "FAIL",
                "prompt": text,
                "contract": contract(ident, props_finish(workers, extra)),
                "reference": program(
                    ident,
                    resources,
                    [
                        fn_main(workers),
                        fn_worker(owner, owner_ok),
                        fn_worker(peer, peer_body),
                        fn_plain(helper, helper_body),
                    ],
                    protection,
                ),
                "bug": program(
                    ident + "_bug",
                    resources,
                    [
                        fn_main(workers),
                        fn_worker(owner, owner_bad),
                        fn_worker(peer, peer_body),
                        fn_plain(helper, helper_body),
                    ],
                    protection,
                ),
            }
        )
    return out


def condvar_tasks() -> list[dict]:
    specs = [
        ("01", "就绪标志", "mtx", "cv", "ready", True, "ready == true", "true", "condvar_notify_all"),
        ("02", "批次开始", "lock", "go", "started", True, "started == true", "true", "condvar_notify"),
        ("03", "水位", "mtx", "cv", "level", False, "level == 1", "1", "condvar_notify_all"),
        ("04", "握手", "pair", "bell", "acked", True, "acked == true", "true", "condvar_notify"),
        ("05", "代次", "mu", "cv", "epoch", False, "epoch == 1", "1", "condvar_notify_all"),
    ]
    out = []
    for index, scene, lock, cv, resource, is_bool, cond, expr, notify in specs:
        workers = ["producer", "consumer"]
        if is_bool:
            shared = var_bool(resource)
            final_value = True
            final_text = f"`{resource}` 最终为真"
        else:
            shared = var_int(resource, 1)
            final_value = 1
            final_text = f"`{resource}` 最终为 1"
        resources = [mutex(lock), condvar(cv), shared]
        protection = [{"var": resource, "lock": lock}]
        ident = f"condvar_{index}"
        extra = [
            {
                "kind": "reachability",
                "id": "signaled",
                "goal": {
                    "kind": "and",
                    "predicates": [
                        *done(workers)["predicates"],
                        {"kind": "var_eq", "resource": resource, "value": final_value},
                    ],
                },
            }
        ]
        out.append(
            {
                "id": ident,
                "family": "condvar",
                "title": f"{scene}：先看条件再等待",
                "reference_outcome": "PASS",
                "bug_outcome": "FAIL",
                "prompt": prompt(
                    f"{scene}：先看条件再等待",
                    (
                        f"`producer` 在 `{lock}` 下把 `{resource}` 写成 {expr}，然后通知 `{cv}`。"
                        f"`consumer` 在同一把锁下等待，直到 `{cond}` 成立，然后返回。"
                        "通知可能发生在等待之前，所以等待前必须先看共享条件。"
                    ),
                    [
                        "恰好一个生产者线程和一个消费者线程。",
                        f"用 `Condvar`，等待时带上 `{lock}` 的守卫。",
                        "不要依赖虚假唤醒。",
                    ],
                    ["不会死锁。", "两个工作线程都返回。", f"{final_text}，并且消费者已经观察到它。"],
                ),
                "contract": contract(ident, props_finish(workers, extra)),
                "reference": program(
                    ident,
                    resources,
                    [
                        fn_main(workers),
                        fn_worker("producer", cond_producer(lock, cv, resource, expr, notify)),
                        fn_worker("consumer", cond_consumer_ok(lock, cv, cond)),
                    ],
                    protection,
                ),
                "bug": program(
                    ident + "_bug",
                    resources,
                    [
                        fn_main(workers),
                        fn_worker("producer", cond_producer(lock, cv, resource, expr, notify)),
                        fn_worker("consumer", cond_consumer_bug(lock, cv)),
                    ],
                    protection,
                ),
            }
        )
    return out


def channel_tasks() -> list[dict]:
    specs = [
        ("01", "会合交接", 0, [1], 1, 2),
        ("02", "单槽邮箱", 1, [7], 1, 2),
        ("03", "两格缓冲", 2, [1, 2], 2, 3),
        ("04", "令牌传递", 0, [5], 1, 2),
        ("05", "单次会合", 0, [9], 1, 2),
    ]
    out = []
    for index, scene, capacity, values, recvs_ok, recvs_bad in specs:
        workers = ["sender", "receiver"]
        resources = [channel("pipe", capacity)]
        ident = f"channel_{index}"
        kind = "会合（容量 0）" if capacity == 0 else f"容量 {capacity} 的有界通道"
        out.append(
            {
                "id": ident,
                "family": "channel",
                "title": f"{scene}：收发条数一致",
                "reference_outcome": "PASS",
                "bug_outcome": "FAIL",
                "prompt": prompt(
                    f"{scene}：收发条数一致",
                    (
                        f"用一条{kind}传递整数 {values}。"
                        f"`sender` 按这个顺序各发送一次，`receiver` 恰好接收 {recvs_ok} 次。"
                        "发送和接收的次数必须相同，否则多出来的一方会永远堵住。"
                    ),
                    [
                        f"通道容量固定为 {capacity}。容量 0 表示发送和接收必须相遇。",
                        "不要关闭通道，也不要增加额外的消息。",
                    ],
                    ["不会死锁。", "`sender` 和 `receiver` 都返回。"],
                ),
                "contract": contract(ident, props_finish(workers)),
                "reference": program(
                    ident,
                    resources,
                    [
                        fn_main(workers),
                        fn_worker("sender", send_body("pipe", values)),
                        fn_worker(
                            "receiver",
                            recv_body("pipe", recvs_ok),
                            locals=[{"name": "msg", "type": "Int"}],
                        ),
                    ],
                    [],
                ),
                "bug": program(
                    ident + "_bug",
                    resources,
                    [
                        fn_main(workers),
                        fn_worker("sender", send_body("pipe", values)),
                        fn_worker(
                            "receiver",
                            recv_body("pipe", recvs_bad),
                            locals=[{"name": "msg", "type": "Int"}],
                        ),
                    ],
                    [],
                ),
            }
        )
    return out


def atomicity_tasks() -> list[dict]:
    specs = [
        ("01", "计数器", "box", "n", 1, 1, 2),
        ("02", "得分", "score_lock", "score", 1, 10, 11),
        ("03", "重量", "scale", "grams", 2, 3, 5),
        ("04", "票数", "poll", "votes", 4, 1, 5),
        ("05", "步数", "step_lock", "steps", 5, 5, 10),
    ]
    out = []
    for index, scene, lock, resource, left_delta, right_delta, expected in specs:
        workers = ["left", "right"]
        resources = [mutex(lock), var_int(resource, expected)]
        protection = [{"var": resource, "lock": lock}]
        local = [{"name": "tmp", "type": "Int"}]
        ident = f"atomicity_{index}"
        out.append(
            {
                "id": ident,
                "family": "atomicity",
                "title": f"{scene}：一次临界区完成加数",
                "reference_outcome": "PASS",
                "bug_outcome": "FAIL",
                "prompt": prompt(
                    f"{scene}：一次临界区完成加数",
                    (
                        f"`left` 把 `{resource}` 增加 {left_delta}，`right` 增加 {right_delta}。"
                        f"每次增加必须在持有 `{lock}` 的同一段临界区里读出旧值并写回新值。"
                        "先读出、释放锁、稍后再写回，会丢掉另一次更新。"
                    ),
                    [
                        "恰好两个工作线程。",
                        f"`{resource}` 的取值范围是 0 到 {expected}。",
                    ],
                    [
                        "不会死锁。",
                        "两个工作线程都返回。",
                        f"无论调度如何，两边都返回时 `{resource}` 等于 {expected}。",
                    ],
                ),
                "contract": contract(
                    ident, props_finish(workers, [bad_final(workers, resource, expected)])
                ),
                "reference": program(
                    ident,
                    resources,
                    [
                        fn_main(workers),
                        fn_worker("left", critical_inc(lock, resource, left_delta)),
                        fn_worker("right", critical_inc(lock, resource, right_delta)),
                    ],
                    protection,
                ),
                "bug": program(
                    ident + "_bug",
                    resources,
                    [
                        fn_main(workers),
                        fn_worker("left", split_inc(lock, resource, left_delta), local),
                        fn_worker("right", split_inc(lock, resource, right_delta), local),
                    ],
                    protection,
                ),
            }
        )
    return out


def semaphore_tasks() -> list[dict]:
    specs = [
        ("01", "打印机", 1, "job", 0, "double"),
        ("02", "车位", 1, "spot", 1, "forget"),
        ("03", "许可池", 2, "permit", 0, "hoard"),
        ("04", "门禁", 1, "door", 1, "forget"),
        ("05", "名额", 1, "seat", 0, "double"),
    ]
    out = []
    for index, scene, count, name, buggy_worker, mode in specs:
        workers = ["a", "b"]
        resources = [semaphore(name, count)]
        ok = [sem_body(name, 1, 1), sem_body(name, 1, 1)]
        if mode == "forget":
            bad_body = sem_body(name, 1, 0)
            bug_text = f"`{'a' if buggy_worker == 0 else 'b'}` 获取一次许可后没有释放。"
        elif mode == "double":
            bad_body = sem_body(name, 2, 2)
            bug_text = f"`{'a' if buggy_worker == 0 else 'b'}` 连续获取两次许可。许可总数只有 {count}。"
        else:
            bad_body = sem_body(name, 2, 0)
            bug_text = f"`a` 连续取走两个许可并且不释放。池里一共只有 {count} 个。"
        bodies_bad = [sem_body(name, 1, 1), sem_body(name, 1, 1)]
        bodies_bad[buggy_worker] = bad_body
        ident = f"semaphore_{index}"
        out.append(
            {
                "id": ident,
                "family": "semaphore",
                "title": f"{scene}：许可成对获取和释放",
                "reference_outcome": "PASS",
                "bug_outcome": "FAIL",
                "prompt": prompt(
                    f"{scene}：许可成对获取和释放",
                    (
                        f"信号量 `{name}` 初始有 {count} 个许可。"
                        "两个工作线程各自获取 1 个许可，做完自己的一步后释放这 1 个许可。"
                        "不要获取第二次，也不要把许可留到线程结束。"
                    ),
                    [
                        "恰好两个工作线程 `a` 和 `b`。",
                        "这一题只检查许可协议，不要求再保护一份共享数据。",
                    ],
                    ["不会死锁。", "`a` 和 `b` 都返回。"],
                ),
                "contract": contract(ident, props_finish(workers)),
                "reference": program(
                    ident,
                    resources,
                    [fn_main(workers), fn_worker("a", ok[0]), fn_worker("b", ok[1])],
                    [],
                ),
                "bug": program(
                    ident + "_bug",
                    resources,
                    [
                        fn_main(workers),
                        fn_worker("a", bodies_bad[0]),
                        fn_worker("b", bodies_bad[1]),
                    ],
                    [],
                ),
            }
        )
        out[-1]["bug_note"] = bug_text
    return out


def main() -> None:
    tasks = build()
    if TASKS.exists():
        for child in TASKS.iterdir():
            if child.is_dir():
                for item in child.iterdir():
                    item.unlink()
                child.rmdir()
    manifest = []
    for task in tasks:
        write_task(task)
        manifest.append(
            {
                "id": task["id"],
                "family": task["family"],
                "title": task["title"],
                "reference_outcome": task["reference_outcome"],
                "bug_outcome": task["bug_outcome"],
            }
        )
    dump(ROOT / "manifest.json", manifest)
    print(f"wrote {len(manifest)} tasks")


if __name__ == "__main__":
    main()
