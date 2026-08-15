<?php

namespace App\Jobs;

class NotifyPostCreatedJob
{
    public $postId;

    public function __construct(int $postId)
    {
        $this->postId = $postId;
    }

    public function handle(): void
    {
        Log::info("Post {$this->postId} created");
    }
}
