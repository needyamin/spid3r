<?php
function run() {
    $n = 2000000;
    $s = 0;
    $i = 0;
    while ($i < $n) {
        $s = $s + $i;
        $i = $i + 1;
    }
    echo $s . PHP_EOL;
}
run();
