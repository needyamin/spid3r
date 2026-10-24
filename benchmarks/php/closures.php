<?php
function run() {
    $n = 20000;
    $total = 0;
    $i = 0;
    while ($i < $n) {
        $adder = function($x) use ($i) {
            return $x + $i;
        };
        $total = $total + $adder($i);
        $i = $i + 1;
    }
    echo $total . PHP_EOL;
}
run();
