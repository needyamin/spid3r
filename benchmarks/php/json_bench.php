<?php
function build($i) {
    return [
        "id" => $i,
        "name" => "item-" . $i,
        "tags" => ["a", "b", "c"]
    ];
}

function run() {
    $n = 2000;
    $count = 0;
    $i = 0;
    while ($i < $n) {
        $text = json_encode(build($i));
        $back = json_decode($text, true);
        $count = $count + $back["id"];
        $i = $i + 1;
    }
    echo $count . PHP_EOL;
}
run();
