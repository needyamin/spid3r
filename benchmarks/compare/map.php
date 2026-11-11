<?php
function build_table($n) {
    $t = [];
    for ($i = 0; $i < $n; $i++) $t["k" . $i] = ($i * 37 + 11) % 997;
    return $t;
}
$t = build_table(64);
$t0 = hrtime(true);
$acc = 0;
for ($r = 0; $r < 100000; $r++) {
    $acc += $t["k" . ($r % 64)];
}
$us = intdiv(hrtime(true) - $t0, 1000);
echo "RESULT $acc 100000\n";
echo "TIME_US $us\n";
