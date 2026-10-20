import io

p = r'C:\Users\needyamin\OneDrive\Desktop\GaliLang\src\native\rt.rs'
raw = io.open(p, encoding='utf-8', newline='').read()

nl = '\r\n'

# Site 1: the AOT runtime's direct native call (argc/argv form).
s1_old = (
    '        Value::Native(n) => {' + nl +
    '            let args = unsafe { args_vec(argc, argv) };' + nl +
    '            let ctx = NativeCtx {' + nl +
    '                sched: Scheduler::shared(),' + nl +
    '            };' + nl +
    '            match (n.func)(&ctx, args) {' + nl +
    '                Ok(v) => leak(v),' + nl +
    '                Err(msg) => rt_throw(&msg),' + nl +
    '            }' + nl +
    '        }'
)
s1_new = s1_old + nl + (
    '        Value::NativeClosure(n) => {' + nl +
    '            let args = unsafe { args_vec(argc, argv) };' + nl +
    '            let ctx = NativeCtx {' + nl +
    '                sched: Scheduler::shared(),' + nl +
    '            };' + nl +
    '            match (n.func)(&ctx, args) {' + nl +
    '                Ok(v) => leak(v),' + nl +
    '                Err(msg) => rt_throw(&msg),' + nl +
    '            }' + nl +
    '        }'
)

# Site 2: the interpreted-callback path of the native runtime.
s2_old = (
    '            Value::Native(n) => {' + nl +
    '                let ctx = crate::value::NativeCtx {' + nl +
    '                    sched: crate::sched::Scheduler::shared(),' + nl +
    '                };' + nl +
    '                (n.func)(&ctx, args)' + nl +
    '            }'
)
s2_new = s2_old + nl + (
    '            Value::NativeClosure(n) => {' + nl +
    '                let ctx = crate::value::NativeCtx {' + nl +
    '                    sched: crate::sched::Scheduler::shared(),' + nl +
    '                };' + nl +
    '                (n.func)(&ctx, args)' + nl +
    '            }'
)

assert raw.count(s1_old) == 1, 'site1 occurrences: %d' % raw.count(s1_old)
assert raw.count(s2_old) == 1, 'site2 occurrences: %d' % raw.count(s2_old)
raw = raw.replace(s1_old, s1_new).replace(s2_old, s2_new)
io.open(p, 'w', encoding='utf-8', newline='').write(raw)
print('rt.rs: both NativeClosure arms inserted')
