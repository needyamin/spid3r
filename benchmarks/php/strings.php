<?php
function run() {
    $n = 30000;
    $s = "";
    $i = 0;
    while ($i < $n) {
        $s = $s . "x";
        $i = $i + 1;
    }
    echo strlen($s) . PHP_EOL;
}
run();
