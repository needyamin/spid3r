<?php
function build_rows($n) {
    $rows = [];
    for ($i = 0; $i < $n; $i++) {
        $rows[] = ["id" => $i, "sku" => "SKU-" . $i,
                   "qty" => ($i % 7) + 1, "price" => ($i * 37) % 1000];
    }
    return $rows;
}
$rows = build_rows(200);
$t0 = hrtime(true);
$total = 0;
$last_len = 0;
for ($r = 0; $r < 600; $r++) {
    $s = json_encode($rows);
    $back = json_decode($s, true);
    $last_len = strlen($s);
    foreach ($back as $row) {
        $total += $row["price"] * $row["qty"] + $row["id"];
    }
}
$us = intdiv(hrtime(true) - $t0, 1000);
echo "RESULT $total $last_len\n";
echo "TIME_US $us\n";
