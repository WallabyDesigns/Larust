<?php

namespace App\Events;

class PostCreated
{
    public function __construct(public int $postId, public int $userId)
    {
    }
}
