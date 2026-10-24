<?php
function run() {
    $n = 2000;
    // Emulate channel message-passing buffer
    $ch = new SplQueue();
    // Producer generates n items
    $i = 0;
    while ($i < $n) {
        $ch->enqueue($i * 2);
        $i = $i + 1;
    }
    // Consumer dequeues and sums
    $total = 0;
    $i = 0;
    while ($i < $n) {
        $total = $total + $ch->dequeue();
        $i = $i + 1;
    }
    echo $total . PHP_EOL;
}
run();
