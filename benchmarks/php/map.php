<?php
function run() {
    $n = 20000;
    $m = [];
    $i = 0;
    while ($i < $n) {
        $m["k" . $i] = $i;
        $i = $i + 1;
    }
    $total = 0;
    $i = 0;
    while ($i < $n) {
        $total = $total + $m["k" . $i];
        $i = $i + 1;
    }
    echo $total . PHP_EOL;
}
run();
