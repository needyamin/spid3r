<?php
function run() {
    $n = 100000;
    $xs = [];
    $i = 0;
    while ($i < $n) {
        $xs[] = $i;
        $i = $i + 1;
    }
    $total = 0;
    while (count($xs) > 0) {
        $total = $total + array_pop($xs);
    }
    echo $total . PHP_EOL;
}
run();
