<?php

namespace App\Http\Controllers;

use App\Models\Post;

class PostController extends Controller
{
    public function index()
    {
        return view('posts.index', ['posts' => Post::all()]);
    }

    public function show(Post $post)
    {
        return view('posts.show', ['post' => $post]);
    }

    public function store(StorePostRequest $request)
    {
        return Post::create($request->validated());
    }

    public function create()
    {
        return view('posts.create');
    }
}
